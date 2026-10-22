//! The expression layout pass: canonical expression text re-wrapped
//! across lines when it exceeds the width budget.
//!
//! The canonical expression text ([`crate::expr`]) is the embedded
//! official parser's own single-line `ToSql` print — correct but, for
//! a long `VALUE` or `WHERE` clause, one hard-to-read line. This
//! module lays that text out across lines at the two writers (DDL
//! codegen and the canonical SDL writer) without touching the canon
//! itself: every writer that renders a human-facing file runs the
//! layout, while text that is consumed positionally (the differ's
//! clause segmentation, diagnostic snippets, the `Expr.canon` string
//! comparisons) keeps the flat form.
//!
//! Design: the official SurrealQL formatter shipped in Surrealist
//! (`@surrealdb/surql-fmt`, Apache-2.0) — a Wadler-Lindig document
//! algebra (`group`/`line`/`softline` with a width-fitting check)
//! whose break points this module mirrors: logical `AND`/`OR` chains
//! break before the operator, function-call argument lists, array and
//! set literals, and object literals break as groups (flat when they
//! fit, one item per line when they do not), and statement blocks
//! always break. The width-fitting printer is a direct port of the
//! official one (flat/break modes with a `fits` simulation), counted
//! in characters.
//!
//! What this module deliberately does NOT do is render SurrealQL: the
//! builder recurses only into the container shapes above, and every
//! leaf prints through the embedded parser's own `ToSql` — the same
//! prints that produced the canon. Precedence parentheses are never
//! re-derived; operand text reuses the upstream predicates verbatim
//! ([`surrealdb_sql::Expr::needs_parentheses`] and [`BindingPower`]).
//! Safety net: a laid-out value is emitted only after a flat render of
//! its document is byte-identical to the canon — any drift (this
//! builder's, or a future embed bump's) degrades to the verbatim canon
//! instead of changing a single token. The one deliberate exception is
//! additive, break-only, and reparse-verified: a broken mixed AND/OR
//! chain wraps with explicit parens around its AND groups
//! ([`explicit_and_group`]).

use surrealdb_sql::{
    BinaryOperator, Expr, Fields, Function, Literal, PrefixOperator,
    data::Data,
    literal::ObjectEntry,
    operator::BindingPower,
    statements::{
        CreateStatement, ForeachStatement, IfelseStatement, SelectStatement, SetStatement,
        UpdateStatement,
    },
};
use surrealdb_types::ToSql as _;

use crate::expr::CanonTree;

/// The default line budget a laid-out expression targets. Counted
/// from column 0 — the writers pass the column the value starts at,
/// so a `key: <value>` line breaks where the finished line would
/// exceed this. 100 (raised from the official formatter's 80 default,
/// 2026-09-28): design files carry deep fn/event nesting, and the
/// budget has to leave room for the statement's own indentation while
/// keeping every line readable on a screen and through a screen
/// reader. The `fmt`/`normalize` `--max-width` flag overrides it per
/// invocation; every other writer keeps this default.
pub(crate) const DEFAULT_MAX_WIDTH: usize = 100;

/// Values shorter than this never lay out, no matter how far past the
/// budget their line already runs: a short call broken across three
/// lines reads worse than the long line it sits on, and that line's
/// length comes from the statement's OTHER clauses — a problem for
/// statement-level layout, not for the value. Values at or past this
/// are the ones whose own structure justifies breaking (`AND` chains,
/// argument lists, the big `THEN` payloads).
const MIN_LAYOUT_LEN: usize = 40;

/// One blank-sensitivity position in a document: a [`Doc::Line`]
/// renders as a space while its group fits and as a newline plus
/// indentation once the group breaks; a softline renders as nothing
/// while flat (the padding inside `(...)`-shaped groups); a hardline
/// always breaks. A break-text renders its parens only while an
/// enclosing group is broken — the flat render, which the identity
/// check compares to the canon, never sees them. A value node is a
/// canonical expression text laid out through [`write_value`] at its
/// render-time position — the flat render of its group keeps the
/// glue-the-skeleton-lay-the-value shape of the pre-skeleton writers
/// (which is byte-identical to verbatim whenever the line fits),
/// while a broken group re-lays it at its final clause column.
enum Doc {
    Text(String),
    BreakText(&'static str),
    Concat(Vec<Doc>),
    Indent(Box<Doc>),
    Group { doc: Box<Doc>, force_break: bool },
    Line,
    SoftLine,
    HardLine,
    Value(String),
}

impl Doc {
    fn text(text: impl Into<String>) -> Doc {
        Doc::Text(text.into())
    }

    fn concat(parts: Vec<Doc>) -> Doc {
        // Single-part and empty concats collapse so the flat-identity
        // check sees plain text where possible.
        match parts.len() {
            0 => Doc::text(""),
            1 => parts.into_iter().next().expect("one part"),
            _ => Doc::Concat(parts),
        }
    }

    fn group(doc: Doc) -> Doc {
        Doc::Group {
            doc: Box::new(doc),
            force_break: false,
        }
    }

    fn broken(doc: Doc) -> Doc {
        Doc::Group {
            doc: Box::new(doc),
            force_break: true,
        }
    }

    /// Comma-joined item documents — flat `a, b, c`, broken one item
    /// per line (the comma stays, the space becomes the break: the
    /// official formatter's `commaSepDocs` shape).
    fn comma_joined(items: Vec<Doc>) -> Doc {
        let mut parts = Vec::new();
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                parts.push(Doc::text(","));
                parts.push(Doc::Line);
            }
            parts.push(item);
        }
        Doc::concat(parts)
    }
}

/// The character count of `text` — the printer's line-width unit (the
/// official formatter counts JS string length; characters are the
/// closest Rust analogue, and the only width-bearing content in an
/// expression is string literals).
fn char_len(text: &str) -> usize {
    text.chars().count()
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Flat,
    Break,
}

/// Emits a newline carrying `base_indent` plus `indent_unit` per
/// nesting level, and reports the new column.
fn newline(out: &mut String, base_indent: &str, indent_unit: &str, indent: usize) -> usize {
    out.push('\n');
    out.push_str(base_indent);
    for _ in 0..indent {
        out.push_str(indent_unit);
    }
    char_len(base_indent) + indent * char_len(indent_unit)
}

/// Prints `doc`. `flat_only` forces every group flat (the
/// flat-identity check); otherwise each group breaks when
/// `force_break` is set or its contents would run past `max_width`
/// from the current position.
fn print(
    doc: &Doc,
    flat_only: bool,
    max_width: usize,
    start_column: usize,
    base_indent: &str,
    indent_unit: &str,
) -> String {
    // The fitting simulation: would this document render flat within
    // `remaining` characters? A forced group or any hardline answers
    // no (a break is coming regardless of width); a line met in flat
    // mode costs one character.
    fn fits(doc: &Doc, mut remaining: isize) -> bool {
        let mut stack = vec![doc];
        while let Some(doc) = stack.pop() {
            match doc {
                Doc::Text(text) => remaining -= char_len(text) as isize,
                // Break-text renders nothing in the flat shape the
                // simulation models.
                Doc::BreakText(_) => {}
                // A value contributes its one-line canon length — the
                // flat shape it rides when the group fits.
                Doc::Value(canon) => remaining -= char_len(canon) as isize,
                Doc::Concat(parts) => {
                    for part in parts.iter().rev() {
                        stack.push(part);
                    }
                }
                Doc::Indent(inner) => stack.push(inner),
                Doc::Group {
                    force_break: true, ..
                } => return false,
                Doc::Group {
                    doc,
                    force_break: false,
                } => stack.push(doc),
                Doc::Line => remaining -= 1,
                Doc::SoftLine => {}
                // No hardline occurs outside a forced group, and the
                // forced group answers no before this is reached —
                // kept as a rejection to mirror the official fits.
                Doc::HardLine => return false,
            }
            if remaining < 0 {
                return false;
            }
        }
        true
    }

    let mut out = String::new();
    let mut position = start_column;
    let mut stack = vec![(0usize, Mode::Break, doc)];
    if flat_only {
        stack[0].1 = Mode::Flat;
    }
    while let Some((indent, mode, doc)) = stack.pop() {
        match doc {
            Doc::Text(text) => {
                out.push_str(text);
                position += char_len(text);
            }
            Doc::BreakText(text) => {
                if mode == Mode::Break {
                    out.push_str(text);
                    position += char_len(text);
                }
            }
            Doc::Concat(parts) => {
                for part in parts.iter().rev() {
                    stack.push((indent, mode, part));
                }
            }
            Doc::Indent(inner) => stack.push((indent + 1, mode, inner)),
            Doc::Group { doc, force_break } => {
                let group_mode = if flat_only {
                    // The identity check flattens even forced groups —
                    // their hardlines render as the single spaces the
                    // canon carries.
                    Mode::Flat
                } else if *force_break {
                    Mode::Break
                } else if mode == Mode::Flat
                    || fits(doc, (max_width as isize).saturating_sub(position as isize))
                {
                    Mode::Flat
                } else {
                    Mode::Break
                };
                stack.push((indent, group_mode, doc));
            }
            Doc::Line => {
                if mode == Mode::Flat {
                    out.push(' ');
                    position += 1;
                } else {
                    position = newline(&mut out, base_indent, indent_unit, indent);
                }
            }
            Doc::SoftLine => {
                if mode == Mode::Break {
                    position = newline(&mut out, base_indent, indent_unit, indent);
                }
            }
            Doc::HardLine => {
                if mode == Mode::Flat {
                    out.push(' ');
                    position += 1;
                } else {
                    position = newline(&mut out, base_indent, indent_unit, indent);
                }
            }
            Doc::Value(canon) => {
                // The nested value's own layout pass, at the position
                // it actually renders: flat mode is the plain
                // `write_value` glue (the fast path keeps the canon
                // verbatim), and a broken group re-lays it with the
                // continuations based at this node's own indentation —
                // a clause value's internal breaks nest under its
                // clause.
                let base = format!("{base_indent}{}", indent_unit.repeat(indent));
                write_value(&mut out, canon, position, &base, indent_unit, max_width);
                position = current_column(&out);
            }
        }
    }
    out
}

/// The current column of `out` — where the next pushed character
/// would land. Writers call this right before pushing a value.
pub(crate) fn current_column(out: &str) -> usize {
    match out.rfind('\n') {
        Some(index) => char_len(&out[index + 1..]),
        None => char_len(out),
    }
}

/// Lays out one canonical expression value onto `out`. `column` is the
/// column the value starts at ([`current_column`] of the output so
/// far); `base_indent` is prefixed to every continuation line (the
/// writer's own indentation, so a broken value lines up under its
/// clause); `indent_unit` is added per document nesting level. Values
/// that fit the budget, shapes no wrapper can read, and any layout
/// whose flat render is not byte-identical to the canon all emit the
/// canon verbatim — the layout can never change a token.
///
/// Canons that carry newlines get one recovery pass before that bail:
/// the upstream print keeps a non-flat detail even in the single-line
/// ToSql (a bracketed IF/ELSE branch holding several statements prints
/// one statement per line), and those newlines are pure inter-token
/// whitespace — the printer escapes any newline inside a strand. The
/// canon is collapsed to one line and laid out normally, accepted only
/// when the collapsed text reparses to the same tree ([`collapse_newlines`]);
/// anything else rides verbatim.
///
/// The one deliberate exception is additive and verified: a broken
/// mixed `AND`/`OR` chain gains precedence-explicit parens around its
/// AND groups ([`explicit_and_group`]). The laid text is accepted only
/// after a reparse ([`crate::expr::canon_tree`]) reads it back to the
/// same tree the canon carries; anything else degrades to the
/// verbatim canon.
pub(crate) fn write_value(
    out: &mut String,
    canon: &str,
    column: usize,
    base_indent: &str,
    indent_unit: &str,
    max_width: usize,
) {
    // Fast path: the canon fits the budget as-is, or is too short for
    // a layout to ever be worth its breaks — no embed parse. A
    // multi-line canon never takes it: its char count spans several
    // physical lines, so the total says nothing about any line's
    // width.
    if !canon.contains('\n')
        && (column + char_len(canon) <= max_width || char_len(canon) < MIN_LAYOUT_LEN)
    {
        out.push_str(canon);
        return;
    }
    let tree = crate::expr::canon_tree(canon);
    // The newline recovery pass (see the doc comment): lay out the
    // collapsed text or ride the multi-line canon verbatim.
    let collapsed;
    let canon: &str = match canon.contains('\n') {
        false => canon,
        true => {
            if matches!(tree, CanonTree::Opaque) {
                out.push_str(canon);
                return;
            }
            collapsed = collapse_newlines(canon);
            if crate::expr::canon_tree(&collapsed) != tree {
                out.push_str(canon);
                return;
            }
            collapsed.as_str()
        }
    };
    let doc = match &tree {
        CanonTree::Expr(expr) => {
            // A statement-shaped value's `CoverStmts` parens live in
            // the canon TEXT, not the AST (the parse drops them):
            // nested contexts always carry them, but a statement-slot
            // capture (an event THEN, a `then:` body element) prints
            // the statement BARE. Read the wrap off the text and
            // render the bare form without parens when it is absent.
            let bare_top =
                statement_text(expr).is_some() && !(canon.starts_with('(') && canon.ends_with(')'));
            if bare_top {
                match stmt_doc(expr) {
                    Some(doc) => doc,
                    None => {
                        out.push_str(canon);
                        return;
                    }
                }
            } else {
                expr_doc(expr)
            }
        }
        CanonTree::Block(elements) => block_doc(elements),
        CanonTree::Opaque => {
            out.push_str(canon);
            return;
        }
    };
    // The safety net: the document's flat render must be the canon to
    // the byte, or the canon rides verbatim.
    let flat = print(&doc, true, max_width, column, base_indent, indent_unit);
    if flat != canon {
        out.push_str(canon);
        return;
    }
    let laid = print(&doc, false, max_width, column, base_indent, indent_unit);
    if !laid.contains('\n') {
        out.push_str(canon);
        return;
    }
    // Whitespace is the only flat↔broken difference unless break-text
    // parens fired; those must reparse to the tree the canon carries
    // (see [`explicit_and_group`]). The reparse only has something to
    // vouch for when the document can add tokens at all.
    if laid != flat && has_break_text(&doc) && crate::expr::canon_tree(&laid) != tree {
        out.push_str(canon);
        return;
    }
    out.push_str(&laid);
}

/// Whether any [`Doc::BreakText`] sits in the document — the only
/// variant that can put non-whitespace differences between the flat
/// and broken renders.
fn has_break_text(doc: &Doc) -> bool {
    match doc {
        Doc::BreakText(_) => true,
        Doc::Concat(parts) => parts.iter().any(has_break_text),
        Doc::Indent(inner) | Doc::Group { doc: inner, .. } => has_break_text(inner),
        Doc::Text(_) | Doc::Line | Doc::SoftLine | Doc::HardLine | Doc::Value(_) => false,
    }
}

/// Folds every newline (and tab) run in the canon into a single
/// space — the upstream print's bracketed multi-statement IF branches
/// are the one non-flat detail of the flat ToSql (see
/// [`write_value`]). Blind by construction: the printer escapes any
/// newline or tab inside a strand, so every such byte is
/// inter-token whitespace — and the caller still reparses the
/// collapsed text against the original tree before trusting it, so a
/// future embed print that breaks the assumption degrades to the
/// verbatim canon rather than mis-laying it.
fn collapse_newlines(canon: &str) -> String {
    let mut out = String::with_capacity(canon.len());
    let mut break_pending = false;
    for ch in canon.chars() {
        if ch == '\n' || ch == '\t' {
            break_pending = true;
        } else if break_pending && ch == ' ' {
            // Spaces inside a break run fold with it — the print's
            // `}`-indent runs must not survive as padding.
        } else {
            if break_pending {
                out.push(' ');
                break_pending = false;
            }
            out.push(ch);
        }
    }
    out
}

// ---- the builder ----

/// Renders one expression in value position. Invariant: the flat
/// render of this document equals what the embedded printer emits for
/// the same tree in the same position (`write_value`'s flat check
/// enforces it against the canon on every value).
///
/// Statement-shaped expressions are parenthesized by the upstream
/// print wherever they appear as values ([`CoverStmts`]); the shapes
/// with real inner structure (IF/ELSE chains, SELECT, UPDATE SET,
/// FOR) get their statement document inside the parens, and the rest
/// of the covered set rides as flat text inside them.
fn expr_doc(expr: &Expr) -> Doc {
    if statement_text(expr).is_some() {
        // A covered statement lays out inside its `CoverStmts` parens
        // when its shape has a document ([`stmt_doc`] family) and
        // rides as flat covered text when it does not.
        let covered = |doc: Option<Doc>| match doc {
            Some(doc) => paren_group(doc),
            None => paren_group(Doc::text(repaired(expr))),
        };
        return match expr {
            Expr::IfElse(ifelse) => paren_group(ifelse_doc(ifelse)),
            Expr::Foreach(foreach) => paren_group(foreach_doc(foreach)),
            Expr::Update(update) => covered(update_doc(update)),
            Expr::Create(create) => covered(create_doc(create)),
            Expr::Select(select) => covered(select_doc(select)),
            _ => covered(None),
        };
    }
    match expr {
        Expr::Binary {
            op:
                op @ (BinaryOperator::And
                | BinaryOperator::Or
                | BinaryOperator::Add
                | BinaryOperator::Equal
                | BinaryOperator::NotEqual),
            ..
        } => chain_doc(expr, op),
        Expr::Prefix { op, expr } => prefix_doc(op, expr),
        Expr::FunctionCall(call) => call_doc(call),
        Expr::Return(output) => Doc::concat(vec![Doc::text("RETURN "), expr_doc(&output.what)]),
        Expr::Literal(Literal::Array(items)) => {
            list_group("[", items.iter().map(expr_doc).collect(), "]")
        }
        Expr::Literal(Literal::Set(items)) => {
            // Upstream paren-wraps a record-id FIRST element (the
            // braces would otherwise re-read as an object), and a
            // single-element set keeps its trailing comma flat (`{1,}`
            // — without it the braces re-read as an object/block).
            let mut padded: Vec<Doc> = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let doc = expr_doc(item);
                if i == 0 && matches!(item, Expr::Literal(Literal::RecordId(_))) {
                    padded.push(paren_group(doc));
                } else {
                    padded.push(doc);
                }
            }
            let mut inner = Doc::comma_joined(padded);
            if items.len() == 1 {
                inner = Doc::concat(vec![inner, Doc::text(",")]);
            }
            list_group("{", vec![inner], "}")
        }
        Expr::Literal(Literal::Object(entries)) => {
            // The upstream non-pretty object print pads the braces
            // (`{ a: 1 }` — literal.rs), so the flat render must carry
            // the same padding or the identity check would fail for
            // every object.
            padded_group(entries.iter().map(entry_doc).collect())
        }
        // Every other shape prints through the embedded printer — the
        // exact text the canon carries, with its own internal
        // parentheses.
        other => Doc::text(repaired(other)),
    }
}

/// One `key: value` object entry — the key through upstream's
/// `EscapeObjectKey` rule (mirrored; embed-pinned in the tests) and
/// the value in covered position (`CoverStmts`, like the object
/// printer's own entry loop).
fn entry_doc(entry: &ObjectEntry) -> Doc {
    Doc::concat(vec![
        Doc::text(object_key(&entry.key)),
        Doc::text(": "),
        expr_doc(&entry.value),
    ])
}

/// The `EscapeObjectKey` rule: bare when ASCII alphanumeric plus
/// underscore, double-quoted through the escaped-writer otherwise
/// (empty keys, digit-leading keys, and the two float sentinels
/// included).
fn object_key(key: &str) -> String {
    let quoted = key.is_empty()
        || key.starts_with(|c: char| c.is_ascii_digit())
        || key.contains(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        || key == "NaN"
        || key == "Infinity";
    if !quoted {
        return key.to_string();
    }
    let mut out = String::new();
    out.push('"');
    crate::keywords::escape_with(&mut out, key, '"');
    out.push('"');
    out
}

/// A `[ … ]` / `{ … }` container: flat when the items fit on the
/// current line, one item per line (indented) once they do not.
fn list_group(open: &str, items: Vec<Doc>, close: &str) -> Doc {
    Doc::group(Doc::concat(vec![
        Doc::text(open),
        Doc::Indent(Box::new(Doc::concat(vec![
            Doc::SoftLine,
            Doc::comma_joined(items),
        ]))),
        Doc::SoftLine,
        Doc::text(close),
    ]))
}

/// The object container: the [`Line`] docs supply the upstream print's
/// brace padding flat (`{ a: 1 }`) and become the breaks broken (one
/// entry per line, the closing brace at the base indentation).
fn padded_group(items: Vec<Doc>) -> Doc {
    Doc::group(Doc::concat(vec![
        Doc::text("{"),
        Doc::Indent(Box::new(Doc::concat(vec![
            Doc::Line,
            Doc::comma_joined(items),
        ]))),
        Doc::Line,
        Doc::text("}"),
    ]))
}

/// A parenthesized group: flat `(x)`, broken with the content on its
/// own indented line.
fn paren_group(content: Doc) -> Doc {
    Doc::group(Doc::concat(vec![
        Doc::text("("),
        Doc::Indent(Box::new(Doc::concat(vec![Doc::SoftLine, content]))),
        Doc::SoftLine,
        Doc::text(")"),
    ]))
}

/// A prefix operator — `!`, `+`/`-`, the `..` ranges, `<kind>` casts.
/// The operand's covering parens mirror the upstream prefix print's
/// own condition exactly (`needs_parentheses`, a strictly looser
/// binding power, the equal-power Range case, and the
/// Negate-over-left-minus cover that keeps `--` from re-lexing), so
/// the flat render is the canon to the byte. A covered operand prints
/// through [`paren_group`] — its broken form is `op(` with the
/// operand's own layout (a wrapped chain, usually) on the indented
/// lines and the close paren back at the operator's column, which is
/// how the long negated guards inside fn bodies wrap.
fn prefix_doc(op: &PrefixOperator, expr: &Expr) -> Doc {
    let inner = expr_doc(expr);
    let bp = BindingPower::for_expr(expr);
    let op_bp = BindingPower::for_prefix_operator(op);
    let covered = expr.needs_parentheses()
        || bp < op_bp
        || bp == op_bp && matches!(bp, BindingPower::Range)
        || *op == PrefixOperator::Negate && expr.has_left_minus();
    if covered {
        Doc::concat(vec![Doc::text(op.to_sql()), paren_group(inner)])
    } else {
        Doc::concat(vec![Doc::text(op.to_sql()), inner])
    }
}

/// Collects the operands of an `AND`/`OR`/`+`/`=`/`!=` chain with
/// their original tree position: same-operator left spines flatten
/// recursively (`a AND b AND c` yields `a, b, c`), while right
/// children push RAW — a right-hand same-operator chain stays one
/// operand (its own [`chain_doc`] group handles it) — so every entry
/// records whether it sat in the parent's right slot. The upstream
/// operand-paren rules are position-sensitive, and an explicitly
/// parenthesised right child can sit anywhere in the flattened list
/// (`a + b + (c - d) + e`).
fn flatten_chain<'a>(expr: &'a Expr, op: &BinaryOperator, out: &mut Vec<(&'a Expr, bool)>) {
    if let Expr::Binary {
        left,
        op: cursor_op,
        right,
    } = expr
        && cursor_op == op
    {
        flatten_chain(left, op, out);
        out.push((right, true));
    } else {
        out.push((expr, false));
    }
}

/// `AND`/`OR`/`+`/`=`/`!=` chains: one breakable group, flat unchanged,
/// or with each operand on its own line led by the operator (the
/// official formatter's shape), continuations one indent unit in.
/// Operand parentheses mirror the upstream binary print's per-operand
/// conditions exactly — `needs_parentheses`, a strictly looser binding
/// power, or the position-aware equal-power sets (a same-power RIGHT
/// child under a left-associative operator only exists because it was
/// explicitly parenthesised — `a + (b - c)` — and the canon carries
/// those parens). Same-operator left spines flatten, so `a AND b AND c`
/// yields `a, b, c` and `a + b + c` breaks at each `+`; a right-hand
/// same-operator chain stays one operand — its own [`chain_doc`]
/// group handles it. One known gap: an Equality-chain spine
/// whose left nodes were hand-parenthesised — `(a = b) = c` — loses
/// those spine parens in the flatten and rides verbatim.
fn chain_doc(expr: &Expr, op: &BinaryOperator) -> Doc {
    let mut operands = Vec::new();
    flatten_chain(expr, op, &mut operands);

    let op_bp = BindingPower::for_binary_operator(op);
    // The upstream equal-power sets: a same-power LEFT child keeps
    // its parens only for Range/Relation/Equality/Power (Range and
    // Power are right-associative; Equality chains cannot reparse
    // without them), and a same-power RIGHT child under a
    // left-associative operator only exists because it was
    // explicitly parenthesised — `a + (b - c)`, `a / (b * c)` — so
    // AddSub/MulDiv join the set there.
    let keeps_parens = |bp: BindingPower, right: bool| -> bool {
        bp == BindingPower::Range
            || bp == BindingPower::Relation
            || bp == BindingPower::Equality
            || (if right {
                bp == BindingPower::AddSub || bp == BindingPower::MulDiv
            } else {
                bp == BindingPower::Power
            })
    };
    let op_text = op.to_sql();
    let mut head = Vec::new();
    let mut tail = Vec::new();
    for (i, (operand, right)) in operands.iter().enumerate() {
        let inner = expr_doc(operand);
        // A covered operand's parens are already in its document
        // ([`expr_doc`]'s covered arm mirrors `CoverStmts`) — adding
        // the operand parens again would double them. The two rules
        // coincide for covered shapes, so this wraps only the rest.
        let bp = BindingPower::for_expr(operand);
        let needs_parens = statement_text(operand).is_none()
            && (operand.needs_parentheses()
                || bp < op_bp
                || bp == op_bp && keeps_parens(bp, *right));
        let operand_doc = if needs_parens {
            paren_group(inner)
        } else if *op == BinaryOperator::Or
            && matches!(
                **operand,
                Expr::Binary {
                    op: BinaryOperator::And,
                    ..
                }
            )
        {
            explicit_and_group(inner)
        } else {
            inner
        };
        if i == 0 {
            head.push(operand_doc);
        } else {
            tail.push(Doc::Line);
            tail.push(Doc::text(op_text.clone()));
            tail.push(Doc::text(" "));
            tail.push(operand_doc);
        }
    }
    head.push(Doc::Indent(Box::new(Doc::concat(tail))));
    Doc::group(Doc::concat(head))
}

/// The precedence-explicit wrap for an AND group inside a broken OR
/// chain: parens that exist only in the broken layout — the flat
/// render (the canon) never carries them, since `AND` binding tighter
/// than `OR` is the parse's own knowledge. With them, the wrapped
/// form shows each group's extent instead of making the reader
/// re-derive the precedence:
///
/// ```text
/// a = 1 AND b = 2
///     OR (c = 3 AND d = 4)
/// ```
///
/// Because the addition changes tokens, [`write_value`] accepts the
/// laid text only after a reparse reads it back to the tree the canon
/// carries — the same operand, wrapped or not.
fn explicit_and_group(content: Doc) -> Doc {
    Doc::concat(vec![Doc::BreakText("("), content, Doc::BreakText(")")])
}

/// The receiver's own print. `Function` has no `ToSql` of its own —
/// the receiver text only exists inside [`surrealdb_sql::FunctionCall`]'s
/// print — so it is read off an empty-argument call, whose print is
/// exactly the receiver followed by `()`.
fn receiver_text(receiver: &Function) -> String {
    let empty = surrealdb_sql::FunctionCall {
        receiver: receiver.clone(),
        arguments: Vec::new(),
    };
    let printed = repaired(&empty);
    printed[..printed.len() - 2].to_string()
}

/// A function call: the receiver verbatim, the argument list a
/// breakable group. Script (`function(a, b) { … }`) and model
/// receivers print their own argument lists — those calls, and any
/// call whose print does not split cleanly at the receiver boundary,
/// stay leaves.
fn call_doc(call: &surrealdb_sql::FunctionCall) -> Doc {
    if call.arguments.is_empty()
        || matches!(call.receiver, Function::Script(_) | Function::Model(_))
    {
        return Doc::text(repaired(call));
    }
    let receiver = receiver_text(&call.receiver);
    let whole = repaired(call);
    if !whole
        .strip_prefix(receiver.as_str())
        .is_some_and(|rest| rest.starts_with('('))
    {
        return Doc::text(whole);
    }
    let args: Vec<Doc> = call.arguments.iter().map(expr_doc).collect();
    Doc::group(Doc::concat(vec![
        Doc::text(receiver),
        Doc::Indent(Box::new(Doc::concat(vec![
            Doc::text("("),
            Doc::SoftLine,
            Doc::comma_joined(args),
        ]))),
        Doc::SoftLine,
        Doc::text(")"),
    ]))
}

/// A statement block (`fn` bodies, `THEN { … }`): one statement per
/// line, always broken. Element text mirrors the upstream block
/// printer's own rules — a record-id literal in the leading position,
/// and any `$param = …` comparison anywhere, print parenthesized so
/// the braces cannot re-read as an object. Statements with inner
/// structure (IF/ELSE chains, FOR bodies, `UPDATE … SET`, RETURN,
/// LET) lay out recursively through [`stmt_doc`]; everything else
/// prints flat.
fn block_doc(elements: &[Expr]) -> Doc {
    if elements.is_empty() {
        // Upstream's empty-block print; nothing to lay out.
        return Doc::text("{;}");
    }
    let mut inner = Vec::new();
    // A single-element block prints without the trailing `;` (the
    // upstream flat shape `{ 1 }`); multi-element bodies carry one
    // after every statement.
    let semi = elements.len() > 1;
    for (i, element) in elements.iter().enumerate() {
        inner.push(Doc::HardLine);
        inner.push(element_doc(element, i == 0));
        if semi {
            inner.push(Doc::text(";"));
        }
    }
    // The closing brace's newline sits OUTSIDE the indent, so `}`
    // lands at the base indentation while the statements sit one unit
    // deeper.
    Doc::broken(Doc::concat(vec![
        Doc::text("{"),
        Doc::Indent(Box::new(Doc::concat(inner))),
        Doc::HardLine,
        Doc::text("}"),
    ]))
}

/// One statement inside a block. The two upstream paren rules
/// (leading record-id, `$param = …` anywhere) wrap the whole element,
/// laid-out or not.
fn element_doc(element: &Expr, leading: bool) -> Doc {
    let record_leading = leading && matches!(element, Expr::Literal(Literal::RecordId(_)));
    let param_assign = matches!(
        element,
        Expr::Binary {
            left,
            op: BinaryOperator::Equal,
            ..
        } if matches!(**left, Expr::Param(_))
    );
    let doc = stmt_doc(element).unwrap_or_else(|| Doc::text(repaired(element)));
    if record_leading || param_assign {
        paren_group(doc)
    } else {
        doc
    }
}

/// The statement shapes that lay out inside a block, in the bare form
/// the upstream block print uses (no `CoverStmts` parens there).
/// `None` prints flat.
fn stmt_doc(element: &Expr) -> Option<Doc> {
    match element {
        Expr::IfElse(ifelse) => Some(ifelse_doc(ifelse)),
        Expr::Foreach(foreach) => Some(foreach_doc(foreach)),
        Expr::Update(update) => update_doc(update),
        Expr::Create(create) => create_doc(create),
        Expr::Select(select) => select_doc(select),
        Expr::Return(output) if output.fetch.is_none() => Some(Doc::concat(vec![
            Doc::text("RETURN "),
            expr_doc(&output.what),
        ])),
        Expr::Let(stmt) => let_doc(stmt),
        _ => None,
    }
}

/// The node's embedded print with the module-call space repaired —
/// upstream's `Function::Module` arm emits `mod:: name::fn(…)` (the
/// one spelling its siblings do not carry), and fmt must not
/// reproduce it ([`crate::lexer::repair_module_call_spaces`]). Every
/// print site that can carry a function call routes through this.
fn repaired<T: surrealdb_types::ToSql + ?Sized>(value: &T) -> String {
    crate::lexer::repair_module_call_spaces(&value.to_sql())
}

/// The flat text for a `CoverStmts`-position expression — the
/// statement-shaped forms parenthesized, everything else bare.
/// [`expr_doc`] is its document form: the covered arm is the
/// `CoverStmts` mirror (statement-shaped expressions parenthesized —
/// the IF/ELSE chain lays out inside its parens — value-shaped ones
/// bare), so every covered position routes through it.
fn cover_text(expr: &Expr) -> String {
    if statement_text(expr).is_some() {
        format!("({})", repaired(expr))
    } else {
        repaired(expr)
    }
}

/// An IF/ELSE chain, in whichever of the two upstream print forms the
/// tree takes. `bracketed()` trees (every branch a block — the api
/// `then:` shape) print `IF <cond> { … } ELSE …` with no END and are
/// always broken (their block branches always are). Everything else
/// prints `IF <cond> THEN <branch> ELSE <close> END` as one breakable
/// group — flat when the whole chain fits the budget (a short inline
/// `IF … THEN … ELSE … END` stays inline), one branch per line when
/// it does not:
///
/// ```text
/// IF <cond>
/// THEN
///   <branch>
/// ELSE IF <cond2>
/// THEN
///   <branch2>
/// ELSE
///   <close>
/// END
/// ```
///
/// A THEN branch that is itself a bracketed IF/ELSE carries the
/// upstream disambiguation parens; other covered branches ride
/// [`expr_doc`].
fn ifelse_doc(ifelse: &IfelseStatement) -> Doc {
    let bracketed = ifelse.bracketed();
    let mut parts = Vec::new();
    for (i, (cond, then)) in ifelse.exprs.iter().enumerate() {
        if i > 0 {
            parts.push(Doc::Line);
            parts.push(Doc::text("ELSE "));
        }
        parts.push(Doc::text("IF "));
        parts.push(expr_doc(cond));
        if bracketed {
            parts.push(Doc::text(" "));
            parts.push(block_branch_doc(then));
        } else {
            // THEN stays at the IF's column; the branch's starting
            // line is INSIDE the indent, so a break there emits the
            // deeper column (the branch's own internal breaks then
            // nest one further).
            parts.push(Doc::Line);
            parts.push(Doc::text("THEN"));
            parts.push(Doc::Indent(Box::new(Doc::concat(vec![
                Doc::Line,
                then_doc(then),
            ]))));
        }
    }
    if let Some(close) = &ifelse.close {
        parts.push(Doc::Line);
        parts.push(Doc::text("ELSE"));
        if bracketed {
            parts.push(Doc::text(" "));
            parts.push(block_branch_doc(close));
        } else {
            parts.push(Doc::Indent(Box::new(Doc::concat(vec![
                Doc::Line,
                expr_doc(close),
            ]))));
        }
    }
    if !bracketed {
        parts.push(Doc::Line);
        parts.push(Doc::text("END"));
    }
    let body = Doc::concat(parts);
    if bracketed {
        Doc::broken(body)
    } else {
        Doc::group(body)
    }
}

/// The THEN branch of a non-bracketed IF. The upstream print writes
/// the branch `{then}` — BARE, with one exception: a bracketed
/// IF/ELSE there carries disambiguation parens (`({then})`, so its
/// ELSE cannot re-bind to the outer chain). No `CoverStmts` parens
/// apply in this position — even statement-shaped branches
/// (`RELATE …`, a nested SELECT) print bare. Expression branches
/// still get their breakable documents (an `Add` chain breaks like
/// any other); their flat render is the same bare text.
fn then_doc(then: &Expr) -> Doc {
    match then {
        Expr::IfElse(inner) if inner.bracketed() => {
            Doc::concat(vec![Doc::text("("), ifelse_doc(inner), Doc::text(")")])
        }
        Expr::IfElse(inner) => ifelse_doc(inner),
        _ if statement_text(then).is_some() => Doc::text(repaired(then)),
        _ => expr_doc(then),
    }
}

/// A bracketed IF's branch: a block (all `bracketed()` branches are)
/// through the block document, broken with the statement on its own
/// line — the flat render stays the upstream `{ stmt }`.
fn block_branch_doc(branch: &Expr) -> Doc {
    match branch {
        Expr::Block(block) => block_doc(&block.0),
        other => Doc::text(repaired(other)),
    }
}

/// A `FOR <param> IN <range> { … }` loop. The block always breaks
/// (its hardlines force the group), one statement per line.
fn foreach_doc(foreach: &ForeachStatement) -> Doc {
    Doc::broken(Doc::concat(vec![
        Doc::text("FOR "),
        Doc::text(foreach.param.to_sql()),
        Doc::text(" IN "),
        expr_doc(&foreach.range),
        Doc::text(" "),
        block_doc(&foreach.block.0),
    ]))
}

/// An `UPDATE <what> SET <assignments>` statement — the plain shape
/// only (no ONLY/WHERE/OUTPUT/TIMEOUT/explain, no MERGE/CONTENT and
/// friends): the assignment list is a breakable group, one assignment
/// per line when the line overflows. Any other UPDATE shape returns
/// `None` and rides flat.
fn update_doc(update: &UpdateStatement) -> Option<Doc> {
    if update.only
        || update.with.is_some()
        || update.cond.is_some()
        || update.output.is_some()
        || !matches!(update.timeout, Expr::Literal(Literal::None))
        || update.explain.is_some()
    {
        return None;
    }
    let Data::SetExpression(assignments) = update.data.as_ref()? else {
        return None;
    };
    if assignments.is_empty() {
        return None;
    }
    let mut whats = String::new();
    for (i, what) in update.what.iter().enumerate() {
        if i > 0 {
            whats.push_str(", ");
        }
        whats.push_str(&cover_text(what));
    }
    let items = assignments
        .iter()
        .map(|assignment| {
            Doc::concat(vec![
                Doc::text(repaired(&assignment.place)),
                Doc::text(" "),
                Doc::text(assignment.operator.to_sql()),
                Doc::text(" "),
                expr_doc(&assignment.value),
            ])
        })
        .collect();
    Some(Doc::group(Doc::concat(vec![
        Doc::text(format!("UPDATE {whats} SET")),
        Doc::Indent(Box::new(Doc::concat(vec![
            Doc::Line,
            Doc::comma_joined(items),
        ]))),
    ])))
}

/// A `CREATE <what> SET <assignments>` statement — the same plain
/// shape [`update_doc`] takes (no ONLY/RETURN/TIMEOUT): the
/// assignment list breaks one assignment per line when the line
/// overflows, which is the shape every fn-body journaling `CREATE`
/// takes. Any other CREATE rides flat.
fn create_doc(create: &CreateStatement) -> Option<Doc> {
    if create.only
        || create.output.is_some()
        || !matches!(create.timeout, Expr::Literal(Literal::None))
    {
        return None;
    }
    let Data::SetExpression(assignments) = create.data.as_ref()? else {
        return None;
    };
    if assignments.is_empty() {
        return None;
    }
    let mut whats = String::new();
    for (i, what) in create.what.iter().enumerate() {
        if i > 0 {
            whats.push_str(", ");
        }
        whats.push_str(&cover_text(what));
    }
    let items = assignments
        .iter()
        .map(|assignment| {
            Doc::concat(vec![
                Doc::text(repaired(&assignment.place)),
                Doc::text(" "),
                Doc::text(assignment.operator.to_sql()),
                Doc::text(" "),
                expr_doc(&assignment.value),
            ])
        })
        .collect();
    Some(Doc::group(Doc::concat(vec![
        Doc::text(format!("CREATE {whats} SET")),
        Doc::Indent(Box::new(Doc::concat(vec![
            Doc::Line,
            Doc::comma_joined(items),
        ]))),
    ])))
}

/// A `SELECT` statement — the plain shape only (projections, FROM
/// targets, an optional WHERE, an optional GROUP BY/ALL; any other
/// clause returns `None` and rides flat — a shape the view grammar
/// routes to `raw`). The official formatter's clause-per-line
/// skeleton as one width-fitting group: flat when the one-line spelling
/// fits the budget, otherwise the projections ride one per line
/// indented under `SELECT` (the official `indentFields` shape, when
/// there are several) and `FROM`/`WHERE`/`GROUP` each take their own
/// line at the statement's indentation — a WHERE chain's own operands
/// nest one unit under it:
///
/// ```text
/// SELECT field_a, field_b
/// FROM invoice
/// WHERE delta > 100
///     AND meta::id($after) != none
/// GROUP BY field_a
/// ```
fn select_doc(select: &SelectStatement) -> Option<Doc> {
    if !select.omit.is_empty()
        || select.with.is_some()
        || select.split.is_some()
        || select.order.is_some()
        || select.limit.is_some()
        || select.start.is_some()
        || select.fetch.is_some()
        || !matches!(select.version, Expr::Literal(Literal::None))
        || !matches!(select.timeout, Expr::Literal(Literal::None))
        || select.for_update
        || select.explain.is_some()
    {
        return None;
    }
    let mut parts: Vec<Doc> = Vec::new();
    // Projections: several ride one per line indented under SELECT
    // (the break point is the document's own Line); a single
    // projection or a `VALUE` projection glues to the SELECT keyword.
    match &select.fields {
        Fields::Select(items) if items.len() > 1 => {
            let docs: Vec<Doc> = items
                .iter()
                .map(|field| Doc::text(repaired(field)))
                .collect();
            parts.push(Doc::text("SELECT"));
            parts.push(Doc::Indent(Box::new(Doc::concat(vec![
                Doc::Line,
                Doc::comma_joined(docs),
            ]))));
        }
        // A single projection or a `VALUE <selector>` glues to the
        // SELECT keyword (its print is the flat canon — the upstream
        // `CoverStmts` wrap rides the selector text).
        _ => {
            parts.push(Doc::text(format!("SELECT {}", repaired(&select.fields))));
        }
    }
    // FROM [ONLY] targets, then each remaining clause on its own
    // line — the clause lines sit at the SAME indent as the broken
    // projections (one unit in), so a select that opens mid-line (a
    // view's `AS SELECT`, a capture) keeps its whole body aligned.
    let clause = |parts: &mut Vec<Doc>, line: Vec<Doc>| {
        parts.push(Doc::Indent(Box::new(Doc::concat(
            std::iter::once(Doc::Line).chain(line).collect(),
        ))));
    };
    let mut from = vec![Doc::text(if select.only { "FROM ONLY " } else { "FROM " })];
    for (i, what) in select.what.iter().enumerate() {
        if i > 0 {
            from.push(Doc::text(", "));
        }
        from.push(Doc::text(cover_text(what)));
    }
    clause(&mut parts, from);
    if let Some(cond) = &select.cond {
        clause(&mut parts, vec![Doc::text("WHERE "), expr_doc(&cond.0)]);
    }
    if let Some(groups) = &select.group {
        let mut line: Vec<Doc> = Vec::new();
        if groups.0.is_empty() {
            line.push(Doc::text("GROUP ALL"));
        } else {
            line.push(Doc::text("GROUP BY "));
            for (i, group) in groups.0.iter().enumerate() {
                if i > 0 {
                    line.push(Doc::text(", "));
                }
                line.push(Doc::text(repaired(&group.0)));
            }
        }
        clause(&mut parts, line);
    }
    Some(Doc::group(Doc::concat(parts)))
}

/// A `LET $name = <value>` statement. Non-word-shaped names bail to
/// the flat print (the upstream escape rule for them is not mirrored
/// here — the flat-identity check guards the guess either way).
fn let_doc(stmt: &SetStatement) -> Option<Doc> {
    if !crate::keywords::word_shaped(stmt.name.as_str()) {
        return None;
    }
    let mut parts = vec![Doc::text(format!("LET ${}", stmt.name.as_str()))];
    if let Some(kind) = &stmt.kind {
        parts.push(Doc::text(format!(": {}", kind.to_sql())));
    }
    parts.push(Doc::text(" = "));
    parts.push(expr_doc(&stmt.what));
    Some(Doc::concat(parts))
}

// ---- the statement skeleton ----

/// One piece of a skeleton segment: verbatim text, a canonical
/// expression value laid out at its render-time column
/// ([`Doc::Value`]), or a break point — flat space, broken newline at
/// the clause's own indent (the comma-separated payload lists' group
/// separator).
pub(crate) enum Seg {
    Text(String),
    Value(String),
    Line,
}

impl Seg {
    pub(crate) fn text(text: impl Into<String>) -> Seg {
        Seg::Text(text.into())
    }

    pub(crate) fn value(canon: impl Into<String>) -> Seg {
        Seg::Value(canon.into())
    }

    pub(crate) fn line() -> Seg {
        Seg::Line
    }
}

/// One clause of a statement skeleton: the key text (leading space
/// included, trailing space where the emission carries one — the flat
/// render is the pre-skeleton emission to the byte) plus its payload
/// pieces.
pub(crate) struct Clause {
    key: String,
    parts: Vec<Seg>,
}

/// A statement skeleton (the surql-fmt `shouldStatementBreak` /
/// clause-per-line model, mirrored onto the DDL emission): the head
/// is glued — the statement keyword, the name, and the header clauses
/// the official formatter's `DEFINE_HEADER_CLAUSES` set keeps on the
/// first line (ON/TYPE/FIELDS/OVERWRITE and SDL's flag riders) — and
/// every other top-level clause is one break candidate. The whole
/// statement is one width-fitting group: when its one-line render
/// (values at their canon lengths) fits the budget, the flat render
/// emits — byte-identical to the pre-skeleton glue — and when it does
/// not, each clause takes its own line one indent unit in, its value
/// pieces laid out at their final columns.
pub(crate) struct Skeleton {
    head: Vec<Seg>,
    clauses: Vec<Clause>,
}

impl Skeleton {
    pub(crate) fn new() -> Skeleton {
        Skeleton {
            head: Vec::new(),
            clauses: Vec::new(),
        }
    }

    /// Head text — the statement spine and its glued clauses. The
    /// writers build runs of it with the same emit helpers as before
    /// (they take a `&mut String`) and hand the finished run here.
    pub(crate) fn push_head_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.head.last_mut() {
            Some(Seg::Text(run)) => run.push_str(text),
            _ => self.head.push(Seg::Text(text.to_string())),
        }
    }

    /// A canonical value glued INTO the head — a fn body, a view's
    /// `AS SELECT` query: laid out by its own document, never a break
    /// candidate.
    pub(crate) fn push_head_value(&mut self, canon: &str) {
        self.head.push(Seg::Value(canon.to_string()));
    }

    /// One clause boundary with its payload. The key is quoted as the
    /// DDL text spells it — a leading space included — but the
    /// boundary itself supplies the separating space (a flat render's
    /// line separator IS one space), so one leading space is stripped
    /// here; a trailing space survives whenever the payload follows
    /// the key on the same line.
    pub(crate) fn clause(&mut self, key: &str, parts: Vec<Seg>) {
        self.clauses.push(Clause {
            key: key.strip_prefix(' ').unwrap_or(key).to_string(),
            parts,
        });
    }

    /// Emits the statement (without its trailing `;\n` — the caller
    /// keeps ownership of the line ending, as before).
    pub(crate) fn write(&self, out: &mut String, max_width: usize) {
        let seg_doc = |seg: &Seg| match seg {
            Seg::Text(text) => Doc::Text(text.clone()),
            Seg::Value(canon) => Doc::Value(canon.clone()),
            Seg::Line => Doc::Line,
        };
        let mut parts: Vec<Doc> = self.head.iter().map(seg_doc).collect();
        for clause in &self.clauses {
            let mut clause_docs = vec![Doc::Line, Doc::Text(clause.key.clone())];
            clause_docs.extend(clause.parts.iter().map(seg_doc));
            parts.push(Doc::Indent(Box::new(Doc::concat(clause_docs))));
        }
        parts.push(Doc::text(";"));
        let doc = Doc::group(Doc::concat(parts));
        let column = current_column(out);
        let flat = print(&doc, true, max_width, column, "", "    ");
        // The flat check reads the RENDERED bytes, not the fits
        // simulation: Doc::Value runs its own layout even in the flat
        // render, so a value that broke internally (a multi-statement
        // fn body, a canon past the budget at its glued column) must
        // force the clause-per-line path even when the one-line
        // spelling would measure short.
        if char_len(&flat) + column <= max_width && !flat.contains('\n') {
            out.push_str(&flat);
            return;
        }
        let laid = print(&doc, false, max_width, column, "", "    ");
        out.push_str(&laid);
    }
}

/// The statement shapes the embedded printer parenthesizes in covered
/// positions — the `CoverStmts` arm — returning their bare text.
/// `RETURN` is the one conditional case (covered only with a FETCH).
fn statement_text(expr: &Expr) -> Option<String> {
    let covered = matches!(
        expr,
        Expr::IfElse(_)
            | Expr::Select(_)
            | Expr::Create(_)
            | Expr::Update(_)
            | Expr::Upsert(_)
            | Expr::Delete(_)
            | Expr::Relate(_)
            | Expr::Insert(_)
            | Expr::Define(_)
            | Expr::Remove(_)
            | Expr::Rebuild(_)
            | Expr::Alter(_)
            | Expr::Info(_)
            | Expr::Foreach(_)
            | Expr::Let(_)
            | Expr::Sleep(_)
            | Expr::Explain { .. }
    );
    if covered {
        return Some(repaired(expr));
    }
    if let Expr::Return(output) = expr
        && output.fetch.is_some()
    {
        return Some(repaired(expr));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout entry as the DDL writer calls it (column 0, 4-space
    /// indent units, no base indent). Returns the laid text.
    fn laid(canon: &str) -> String {
        let mut out = String::new();
        write_value(&mut out, canon, 0, "", "    ", DEFAULT_MAX_WIDTH);
        out
    }

    /// The canon must ride verbatim when it fits the budget or is
    /// shorter than the layout floor.
    #[test]
    fn short_and_fitting_values_stay_verbatim() {
        let mut out = String::new();
        write_value(
            &mut out,
            "a AND b AND c",
            40,
            "    ",
            "    ",
            DEFAULT_MAX_WIDTH,
        );
        assert_eq!(out, "a AND b AND c", "fits the budget");
        let mut out = String::new();
        write_value(
            &mut out,
            "string::is_email($value)",
            60,
            "    ",
            "    ",
            DEFAULT_MAX_WIDTH,
        );
        assert_eq!(out, "string::is_email($value)", "under the layout floor");
    }

    /// The flagship shape: a long `AND`/`OR` chain breaks before each
    /// operator, one operand per line, nothing else changed.
    #[test]
    fn logical_chains_break_before_the_operator() {
        let canon = "a.status == 'some genuinely old status value' OR b.status == 'old' OR c.status == 'old' OR d.flag == 'very stale indeed'";
        assert_eq!(
            laid(canon),
            "a.status == 'some genuinely old status value'\n    OR b.status == 'old'\n    OR c.status == 'old'\n    OR d.flag == 'very stale indeed'"
        );
    }

    /// `AND` binds tighter than `OR`: an `OR` operand of an `AND`
    /// chain must keep its parentheses when the chain breaks — the
    /// broken layout has to reparse to the same tree.
    #[test]
    fn or_under_and_keeps_its_parens() {
        // The canon for `a AND (b OR c AND d) AND …` — long enough to
        // break the AND chain.
        let canon = "alpha = 1 AND (beta = 2 OR gamma = 3 AND delta = 4) AND epsilon = 5 AND zeta = 6 AND eta = 7 AND theta = 8";
        let out = laid(canon);
        // Operator-led continuation lines: the indent IS the flat
        // separator, so trimming each line and joining with one space
        // reconstructs the canon.
        let flat = out
            .lines()
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(flat, canon, "the layout moved more than whitespace");
        assert!(
            out.contains("\n    AND (beta = 2 OR"),
            "the OR operand lost its covering parens: {out}"
        );
    }

    /// A same-power RIGHT child in an AddSub chain keeps the parens
    /// the canon carries — `a + (b - c)`: dropping them would reparse
    /// left-associatively and change the value — and an explicitly
    /// parenthesised right child can sit anywhere in the flattened
    /// operand list, not just last.
    #[test]
    fn same_power_right_children_keep_their_parens() {
        let canon = "'first operand of a genuinely long chain' + 'second' + (b - c) + 'fourth operand completes the overflow'";
        let out = laid(canon);
        let flat = out
            .lines()
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(flat, canon, "the layout moved more than whitespace");
        assert!(out.contains("\n    + (b - c)"), "parens lost: {out}");
        let canon = "alpha + beta + (gamma + delta) + 'the final long string operand that pushes this chain past any reasonable budget for sure'";
        let out = laid(canon);
        assert!(
            out.contains("\n    + (gamma + delta)"),
            "the same-operator right child lost its parens: {out}"
        );
    }

    /// Argument lists and arrays break one item per line — but only
    /// where a group actually overflows: the outer argument list breaks
    /// while the array (66 chars at its position) still fits. The flat
    /// render is byte-identical to the canon (enforced inside
    /// `write_value` — a drift lands as the verbatim canon instead).
    #[test]
    fn containers_break_one_item_per_line() {
        let canon = "string::join('-', [string::lowercase($first_name), string::lowercase($last_name), string::lowercase($middle_name)])";
        assert_eq!(
            laid(canon),
            "string::join(\n    '-',\n    [string::lowercase($first_name), string::lowercase($last_name), string::lowercase($middle_name)]\n)"
        );
        let canon = "string::join('-', [string::lowercase($first_name), string::lowercase($last_name), string::lowercase($middle_name), 'a final suffix element'])";
        assert_eq!(
            laid(canon),
            "string::join(\n    '-',\n    [\n        string::lowercase($first_name),\n        string::lowercase($last_name),\n        string::lowercase($middle_name),\n        'a final suffix element'\n    ]\n)"
        );
    }

    /// Multi-statement fn bodies print one statement per line — the
    /// forced-break shape — while the flat render stays byte-identical
    /// to the upstream block print (the identity check flattens
    /// hardlines and forced groups).
    #[test]
    fn statement_blocks_break_one_statement_per_line() {
        let canon = "{ UPDATE a_x SET done = false, count = 1; UPDATE b_x SET done = false, count = 2; UPDATE c_x SET done = false, count = 3; }";
        assert_eq!(
            laid(canon),
            "{\n    UPDATE a_x SET done = false, count = 1;\n    UPDATE b_x SET done = false, count = 2;\n    UPDATE c_x SET done = false, count = 3;\n}"
        );
    }

    /// The IF/ELSE IF ladder — the shape a careful hand gives a
    /// priority lookup: one branch per line, nested IFs indented under
    /// their THEN, the throw block riding bare in ELSE position. A
    /// nested chain that FITS the budget stays inline inside the
    /// broken ladder (the short inline idiom).
    #[test]
    fn ifelse_ladders_break_one_branch_per_line() {
        let canon = crate::expr::canonicalize(
            "IF $lang = 'EnUS' THEN IF $priority = 'high' THEN 'High' ELSE IF $priority = 'medium' THEN 'Medium' ELSE 'Low' END ELSE { THROW 'lang' } END",
            crate::expr::Slot::ParamValue,
            crate::expr::Position::ORIGIN,
        )
        .expect("the ladder parses");
        let out = laid(&canon);
        assert_eq!(
            out,
            "(\n    IF $lang = 'EnUS'\n    THEN\n        IF $priority = 'high' THEN 'High' ELSE IF $priority = 'medium' THEN 'Medium' ELSE 'Low' END\n    ELSE\n        { THROW 'lang' }\n    END\n)"
        );
        let long = crate::expr::canonicalize(
            "IF $lang = 'EnUS' THEN IF $priority = 'high' THEN 'the High priority label' ELSE IF $priority = 'medium' THEN 'the Medium priority label' ELSE 'the Low priority label' END ELSE { THROW 'unknown language' } END",
            crate::expr::Slot::ParamValue,
            crate::expr::Position::ORIGIN,
        )
        .expect("the long ladder parses");
        assert_eq!(
            laid(&long),
            "(\n    IF $lang = 'EnUS'\n    THEN\n        IF $priority = 'high'\n        THEN\n            'the High priority label'\n        ELSE IF $priority = 'medium'\n        THEN\n            'the Medium priority label'\n        ELSE\n            'the Low priority label'\n        END\n    ELSE\n        { THROW 'unknown language' }\n    END\n)"
        );
    }

    /// The bracketed form (every branch a block — the api `then:`
    /// shape): `IF <cond> {` with the statement on its own line and
    /// the close at the IF's column. No THEN/END keywords exist in
    /// this form.
    #[test]
    fn bracketed_ifelse_breaks_one_statement_per_branch() {
        let canon = "{ IF !crypto::argon2::compare($auth.password_hash, $request.body.current_password) { RETURN { status: 400, body: { error: 'wrong_current' } } } }";
        assert_eq!(
            laid(canon),
            "{\n    IF !crypto::argon2::compare($auth.password_hash, $request.body.current_password) {\n        RETURN { status: 400, body: { error: 'wrong_current' } }\n    }\n}"
        );
    }

    /// String-concatenation chains break at each `+`, one operand per
    /// line with a continuation indent — the task-summary shape.
    #[test]
    fn concat_chains_break_at_plus() {
        let out = laid(
            "'Details\\n' + '\\n' + 'Description:\\n' + ($task.description ?? 'N/A') + fn::locale::priority_label($user.language, $task.priority)",
        );
        assert_eq!(
            out,
            "'Details\\n'\n    + '\\n'\n    + 'Description:\\n'\n    + ($task.description ?? 'N/A')\n    + fn::locale::priority_label($user.language, $task.priority)"
        );
    }

    /// `UPDATE … SET` breaks one assignment per line under the SET.
    #[test]
    fn update_set_breaks_one_assignment_per_line() {
        let out = laid(
            "UPDATE $auth SET needs_new_password = false, password_hash = crypto::argon2::generate($request.body.new_password)",
        );
        assert_eq!(
            out,
            "UPDATE $auth SET\n    needs_new_password = false,\n    password_hash = crypto::argon2::generate($request.body.new_password)"
        );
    }

    /// A SELECT lays out clause-per-line once its one-line spelling
    /// passes the budget — the official formatter's view skeleton:
    /// FROM and WHERE take their own lines at the same indent as the
    /// broken projections, and a WHERE chain's operands break one
    /// unit in when the line itself overflows.
    #[test]
    fn select_where_chain_breaks_inside_the_statement() {
        let out = laid(
            "SELECT id FROM user WHERE phone_number.number = $phone_number AND crypto::argon2::compare(password_hash, $password)",
        );
        assert_eq!(
            out,
            "SELECT id\n    FROM user\n    WHERE phone_number.number = $phone_number AND crypto::argon2::compare(password_hash, $password)"
        );
        // A WHERE chain that overflows its own clause line breaks one
        // unit in, under the WHERE.
        let out = laid(
            "SELECT id FROM user WHERE phone_number.number = $the_full_phone_number_value AND crypto::argon2::compare(password_hash, $the_submitted_password)",
        );
        assert!(
            out.contains("WHERE phone_number.number = $the_full_phone_number_value\n        AND crypto::argon2::compare"),
            "the chain nests under WHERE: {out}"
        );
    }

    /// The paren-wrapped value form (a `signin:` capture — the
    /// `CoverStmts` parens ride in the canon): the SELECT lays out
    /// clause-per-line inside its parens, the close paren back at the
    /// value's column.
    #[test]
    fn paren_wrapped_select_lays_out_inside_the_parens() {
        let out = laid(
            "(SELECT id FROM user WHERE phone_number.number = $phone_number AND crypto::argon2::compare(password_hash, $password))",
        );
        assert_eq!(
            out,
            "(\n    SELECT id\n        FROM user\n        WHERE phone_number.number = $phone_number\n            AND crypto::argon2::compare(password_hash, $password)\n)"
        );
    }

    /// A FOR body breaks one statement per line under the loop head.
    #[test]
    fn foreach_body_breaks_one_statement_per_line() {
        let canon = "{ FOR $t IN array::complement(<array> $before.tags, <array> $after.tags) { LET $_ = fn::tags::delete_if_unreferenced($t, NONE) } }";
        assert_eq!(
            laid(canon),
            "{\n    FOR $t IN array::complement(<array> $before.tags, <array> $after.tags) {\n        LET $_ = fn::tags::delete_if_unreferenced($t, NONE)\n    }\n}"
        );
    }

    /// A short inline IF rides flat inside a broken parent — the
    /// `(IF $task.completed THEN 'Yes' ELSE 'No' END)` idiom stays on
    /// one line even when the surrounding concat chain breaks.
    #[test]
    fn short_inline_chains_stay_inline_in_broken_parents() {
        let out = laid(
            "'a long enough string literal to push the line past the budget for sure' + (IF $task.completed THEN 'Yes' ELSE 'No' END)",
        );
        assert!(
            out.contains("(IF $task.completed THEN 'Yes' ELSE 'No' END)"),
            "the short IF must stay inline: {out}"
        );
        assert_eq!(out.matches('\n').count(), 1, "one break only: {out}");
    }

    /// The layout's output is a fixed point of the PIPELINE round
    /// trip: the writers only ever see canonicalize output, so the
    /// second pass re-reads the laid text through the embedded parser
    /// first (this drops the break-only AND-group parens — a canon
    /// never carries them — and re-multi-lines the bracketed IF
    /// branches the collapse folded). The laid text must be that
    /// loop's fixed point.
    #[test]
    fn the_layout_is_idempotent() {
        for canon in [
            "a.status == 'old' OR b.status == 'old' OR c.status == 'old' OR d.flag",
            "alpha = 1 AND beta = 2 OR gamma = 3 AND delta = 4 OR epsilon = 5 AND zeta = 6 OR eta = 7 AND theta = 8 OR iota = 9 AND kappa = 10",
            "{ IF !($kind INSIDE ['text', 'email', 'phone', 'time', 'select', 'radio'] AND $row.value_text != NONE OR $kind = 'number' AND $row.value_number != NONE OR $kind INSIDE ['date', 'datetime'] AND $row.value_datetime != NONE OR $kind = 'item' AND $row.value_item != NONE) { THROW 'the filled variant does not match the definition kind' } }",
            "{ LET $id = $after.id; IF $before = NONE {\nCREATE item_event SET item = $id, kind = 'mint', `by` = $actor, data = { code: $after.code_string, stage: $after.stage };\nUPDATE $id SET stage_since = time::now();\nRETURN NONE;\n}; }",
            "string::join('-', [string::lowercase($first), string::lowercase($last), string::lowercase($middle)])",
            "{ name: 'surform', tags: ['sdl', 'surrealdb', 'schema', 'compiler'], nested: { deep: true, deeper: false, deepest: 1 } }",
        ] {
            let once = laid(canon);
            let recanon = match crate::expr::canon_tree(&once) {
                CanonTree::Expr(expr) => repaired(&expr),
                CanonTree::Block(elements) => repaired(&surrealdb_sql::Block(elements.clone())),
                CanonTree::Opaque => once.clone(),
            };
            let twice = laid(&recanon);
            assert_eq!(once, twice, "not idempotent: {canon}");
        }
    }

    /// The object-key escape rule mirrored from upstream's
    /// `EscapeObjectKey`, pinned against the embed's own canon for
    /// keys that need quoting — and pinned BROKEN: the object must
    /// actually lay out (the flat render carries the upstream brace
    /// padding `{ a: 1 }`, so a stale builder would fall back to the
    /// verbatim canon and this assertion would fail).
    #[test]
    fn object_keys_escape_like_the_embedded_print() {
        let canon = crate::expr::canonicalize(
            "{'key with space': 'a very long value that will not need any internal breaking', plain: 2, '123': 'another long value which stays whole as a single literal token', 'NaN': 4, '': 5}",
            crate::expr::Slot::FieldValue,
            crate::expr::Position::ORIGIN,
        )
        .expect("the object parses in the field-value slot");
        let out = laid(&canon);
        for expected in [
            "\"key with space\": 'a very long value that will not need any internal breaking'",
            "plain: 2",
            "\"123\": 'another long value which stays whole as a single literal token'",
            "\"NaN\": 4",
            "\"\": 5",
        ] {
            assert!(out.contains(expected), "missing {expected} in: {out}");
        }
        assert!(out.contains('\n'), "the object did not lay out: {out}");
        assert!(
            out.starts_with("{\n"),
            "broken objects open their brace: {out}"
        );
        assert!(
            out.ends_with("}"),
            "broken objects close their brace: {out}"
        );
    }

    /// Shapes no wrapper reads (view queries) ride verbatim — the pass
    /// can never change a token it cannot read back.
    #[test]
    fn unrenderable_shapes_fall_back_to_the_canon() {
        let view = "SELECT value, meta::id($after) AS who FROM invoice WHERE delta > 100 AND delta < 200 AND meta::id($after) != none FETCH delta";
        let mut out = String::new();
        write_value(&mut out, view, 0, "    ", "    ", DEFAULT_MAX_WIDTH);
        assert_eq!(out, view, "the view canon must ride verbatim");
    }

    /// A set whose first element is a record-id literal keeps the
    /// upstream paren wrap (`{(person:adamantium), …}` — without it
    /// the braces re-read as an object) and still lays out.
    #[test]
    fn sets_keep_the_leading_record_id_parens() {
        let canon = crate::expr::canonicalize(
            "{(person:adamantium), 'a set element long enough to force the layout to break it across lines', 'one more element for good measure'}",
            crate::expr::Slot::FieldValue,
            crate::expr::Position::ORIGIN,
        )
        .expect("the set parses in the field-value slot");
        let out = laid(&canon);
        assert!(
            out.contains("(person:adamantium),"),
            "the leading record-id lost its parens or its comma: {out}"
        );
        assert!(out.starts_with("{\n"), "the set did not lay out: {out}");
    }

    /// A multi-line canon — the upstream print emits one statement per
    /// line for a bracketed IF branch holding several — collapses to
    /// one line and lays out as a normal fn body: the block breaks,
    /// the `CREATE … SET` assignment list wraps one assignment per
    /// line.
    #[test]
    fn multi_line_canon_collapses_and_lays_out() {
        let canon = "{ LET $id = $after.id; IF $before = NONE {\nCREATE item_event SET item = $id, kind = 'mint', `by` = $actor, data = { code: $after.code_string, stage: $after.stage };\nUPDATE $id SET stage_since = time::now();\nRETURN NONE;\n}; }";
        assert_eq!(
            laid(canon),
            "{\n    LET $id = $after.id;\n    IF $before = NONE {\n        CREATE item_event SET\n            item = $id,\n            kind = 'mint',\n            `by` = $actor,\n            data = { code: $after.code_string, stage: $after.stage };\n        UPDATE $id SET stage_since = time::now();\n        RETURN NONE;\n    };\n}"
        );
    }

    /// A long single-line `CREATE … SET` breaks one assignment per
    /// line under the SET — the same shape [`update_doc`] gives
    /// UPDATE.
    #[test]
    fn create_set_breaks_one_assignment_per_line() {
        let out = laid(
            "CREATE item_event SET item = $id, kind = 'stage', `by` = $actor, data = { from: $before.stage, to: $after.stage }",
        );
        assert_eq!(
            out,
            "CREATE item_event SET\n    item = $id,\n    kind = 'stage',\n    `by` = $actor,\n    data = { from: $before.stage, to: $after.stage }"
        );
    }

    /// A strand carrying a raw newline is the collapse's one
    /// red-line: folding it would change the string's value, and the
    /// reparse check catches the tree drift — the canon rides
    /// verbatim, newlines and all.
    #[test]
    fn strand_with_raw_newline_rides_verbatim() {
        let canon = "{ 'a raw\nnewline inside a strand literal keeps the body verbatim' }";
        let mut out = String::new();
        write_value(&mut out, canon, 40, "    ", "    ", DEFAULT_MAX_WIDTH);
        assert_eq!(out, canon, "the collapse must not touch strand content");
    }

    /// A negated guard — `!( seven OR-joined AND groups )` inside a
    /// fn-body IF — wraps as `!(` with one OR group per line, each AND
    /// group in explicit parens, the close paren back at the IF's
    /// column. The first group is long enough to overflow the budget
    /// at its position, so its own comparisons break inside the group
    /// parens.
    #[test]
    fn negated_mixed_guard_wraps_one_or_group_per_line() {
        let canon = "{ IF !($kind INSIDE ['text', 'email', 'phone', 'time', 'select', 'radio'] AND $row.value_text != NONE OR $kind = 'number' AND $row.value_number != NONE OR $kind INSIDE ['date', 'datetime'] AND $row.value_datetime != NONE OR $kind = 'item' AND $row.value_item != NONE) { THROW 'the filled variant does not match the definition kind' } }";
        assert_eq!(
            laid(canon),
            "{\n    IF !(\n        ($kind INSIDE ['text', 'email', 'phone', 'time', 'select', 'radio']\n            AND $row.value_text != NONE)\n            OR ($kind = 'number' AND $row.value_number != NONE)\n            OR ($kind INSIDE ['date', 'datetime'] AND $row.value_datetime != NONE)\n            OR ($kind = 'item' AND $row.value_item != NONE)\n    ) {\n        THROW 'the filled variant does not match the definition kind'\n    }\n}"
        );
    }

    /// A mixed `AND`/`OR` chain that breaks gains explicit parens
    /// around every AND group — the wrapped form shows the precedence
    /// instead of asking the reader to re-derive it. The flat chain
    /// (the canon) carries none.
    #[test]
    fn mixed_or_chain_parenthesizes_its_and_groups() {
        let canon = "alpha = 1 AND beta = 2 OR gamma = 3 AND delta = 4 OR epsilon = 5 AND zeta = 6 OR eta = 7 AND theta = 8 OR iota = 9 AND kappa = 10";
        assert_eq!(
            laid(canon),
            "(alpha = 1 AND beta = 2)\n    OR (gamma = 3 AND delta = 4)\n    OR (epsilon = 5 AND zeta = 6)\n    OR (eta = 7 AND theta = 8)\n    OR (iota = 9 AND kappa = 10)"
        );
    }

    /// A negated operand wraps inside its covering parens — the
    /// operator keeps its column, the operand's own layout (here a
    /// concatenation chain) breaks inside.
    #[test]
    fn negated_concat_breaks_inside_the_prefix_parens() {
        let canon = "-(string::join(',', $tags) + 'a long enough suffix operand to push this line well past the layout budget for sure')";
        assert_eq!(
            laid(canon),
            "-(\n    string::join(',', $tags)\n        + 'a long enough suffix operand to push this line well past the layout budget for sure'\n)"
        );
    }

    /// The explicit AND-group parens are break-only: the same chain,
    /// short enough to fit, rides verbatim — the canon's own spelling.
    #[test]
    fn fitting_mixed_chain_stays_verbatim() {
        let mut out = String::new();
        write_value(
            &mut out,
            "alpha = 1 AND beta = 2 OR gamma = 3 AND delta = 4",
            40,
            "    ",
            "    ",
            DEFAULT_MAX_WIDTH,
        );
        assert_eq!(out, "alpha = 1 AND beta = 2 OR gamma = 3 AND delta = 4");
    }

    /// The width parameter governs the break: the same chain stays
    /// verbatim under a wide budget and breaks under a narrow one.
    #[test]
    fn the_width_parameter_governs_the_break() {
        let canon = "alpha = 1 AND beta = 2 OR gamma = 3 AND delta = 4";
        let mut out = String::new();
        write_value(&mut out, canon, 0, "    ", "    ", 120);
        assert_eq!(out, canon, "fits a 120 budget");
        let mut out = String::new();
        write_value(&mut out, canon, 0, "", "    ", 40);
        // A mixed AND/OR chain that breaks carries the break-only
        // AND-group parens (explicit_and_group).
        assert_eq!(
            out, "(alpha = 1 AND beta = 2)\n    OR (gamma = 3 AND delta = 4)",
            "breaks under a 40 budget"
        );
    }

    /// A `<kind>` cast prints its operand with the upstream covering
    /// rules too — the flat render keeps the canon's space after the
    /// cast (`{op} ` carries it), and a cast over a chain breaks
    /// inside the parens.
    #[test]
    fn cast_operand_prints_with_the_upstream_cover() {
        let canon =
            "<int> $some_long_parameter_name_that_carries_the_count_of_pending_records_here";
        assert_eq!(laid(canon), canon, "a plain cast rides as written");
        let canon = "<int> ($pending_count_from_the_queue_table + $retry_counter_accumulator_field_value_tracked_across_retries)";
        assert_eq!(
            laid(canon),
            "<int> (\n    $pending_count_from_the_queue_table\n        + $retry_counter_accumulator_field_value_tracked_across_retries\n)"
        );
    }

    /// The statement skeleton: a fitting statement rides the flat
    /// glue byte-for-byte (the boundary's line separator IS the flat
    /// space, and the key's leading space is stripped).
    #[test]
    fn fitting_skeleton_rides_flat() {
        let mut sk = Skeleton::new();
        sk.push_head_text("DEFINE FIELD OVERWRITE email ON account TYPE string");
        sk.clause(" ASSERT ", vec![Seg::text("string::is_email($value)")]);
        let mut out = String::new();
        sk.write(&mut out, DEFAULT_MAX_WIDTH);
        assert_eq!(
            out,
            "DEFINE FIELD OVERWRITE email ON account TYPE string \
             ASSERT string::is_email($value);"
        );
    }

    /// A statement past the budget lays clause-per-line, one indent
    /// unit in; a clause value whose chain overflows even its own
    /// clause line breaks one further unit in, under its clause.
    #[test]
    fn overflowing_skeleton_breaks_clause_per_line() {
        let mut sk = Skeleton::new();
        sk.push_head_text("DEFINE FIELD OVERWRITE status ON invoice TYPE string");
        sk.clause(
            " VALUE ",
            vec![Seg::value(
                "$value OR 'pending submission pipeline stage' OR 'approved by regional manager' OR 'rejected by finance'",
            )],
        );
        sk.clause(" READONLY", Vec::new());
        let mut out = String::new();
        sk.write(&mut out, DEFAULT_MAX_WIDTH);
        assert_eq!(
            out,
            "DEFINE FIELD OVERWRITE status ON invoice TYPE string\n    \
             VALUE $value\n        OR 'pending submission pipeline stage'\n        \
             OR 'approved by regional manager'\n        OR 'rejected by finance'\n    \
             READONLY;"
        );
    }

    /// A head value piece (a fn body) lays out glued to the head and
    /// the trailing clauses break after its close.
    #[test]
    fn head_value_glues_and_clauses_break_after_it() {
        let mut sk = Skeleton::new();
        sk.push_head_text("DEFINE FUNCTION OVERWRITE fn::ping() ");
        sk.push_head_value("{ RETURN 1 }");
        sk.clause(
            " COMMENT ",
            vec![Seg::text(
                "'the ping probe'; and more text to push this line over the budget",
            )],
        );
        let mut out = String::new();
        sk.write(&mut out, DEFAULT_MAX_WIDTH);
        assert_eq!(
            out,
            "DEFINE FUNCTION OVERWRITE fn::ping() { RETURN 1 }\n    \
             COMMENT 'the ping probe'; and more text to push this line over the budget;"
        );
    }
}
