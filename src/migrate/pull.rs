//! `migrate pull` — the interchange's in-tool producer (issue #78):
//! fetches the database's rendered INFO over the same HTTP
//! `POST /sql` transport every live command dials through
//! ([`super::transport`]), and writes the interchange files the
//! `--info` mode consumes — the on-ramp `normalize` and `migrate
//! generate` point at, in place of a hand-rolled shell loop.
//!
//! # The scope (v1)
//!
//! The three session scopes the `--info` tags name, plus one
//! `INFO FOR TABLE` per table the db result carries:
//!
//! - `INFO FOR ROOT` → `surform-info-root.txt` (the `root=` tag),
//! - `INFO FOR NS` → `surform-info-ns-<ns>.txt` (the `ns=<name>=`
//!   tag),
//! - `INFO FOR DB` → `surform-info-db-<ns>-<db>.txt` (the
//!   `db=<ns>/<name>=` tag) — with every table's `INFO FOR TABLE`
//!   object assembled under the reserved [`TABLE_CHILDREN_KEY`]
//!   sibling key, the exact shape `tests/common`'s interchange
//!   fixture pins and `ReadModel::from_interchange` ingests (the
//!   `--info` reader walks the text back into that model; without
//!   the children the compare could not see field-level drift).
//!
//! The `surform-info-` prefix keeps the files out of hand-written
//! interchange files' way: a re-pull REFRESHES its own three names
//! and never touches anything else. Every fetch completes before the
//! first write — a wire refusal leaves the directory untouched — and
//! a write that fails midway leaves the files already written fresh
//! (they are pull's own outputs; re-running the command repairs the
//! set).
//!
//! # The printed command (issue #100)
//!
//! The ready-made next command is DATABASE-scoped: one `--info` tag,
//! the database's. The root and namespace files ride the printed
//! wider-compares line instead — adding their tags is a deliberate
//! act, never part of the pasted default. The reason is structural:
//! a schema file does not manage the server's root users or the
//! default config (the secrets rule keeps them out of SDL), so
//! whenever root-scope interchange joins a compare those statements
//! classify as removals — a three-tag happy path would bake the
//! server's administrative identity into the default next step.
//! The advice lines carry the classification fact, so the wider
//! compares arrive with their hazard named.
//!
//! # The scopes leg (issue #103)
//!
//! The scopes leg still sends the whole three-statement batch —
//! `INFO FOR ROOT; INFO FOR NS; INFO FOR DB;` in one POST — but the
//! elements classify per scope instead of refusing wholesale: a scope
//! the credential cannot read answers the engine's permission
//! refusal (the stable `NotAllowed` kind — probed in-process,
//! `tests/engine_scope_permissions.rs`: a db-level credential is
//! refused on ROOT and NS while its DB read is OK, and an ns-level
//! credential stops one scope shorter) and the pull proceeds with the
//! readable scopes. A refused scope writes no interchange file, and
//! the printed advice names it (a credential that can read the
//! missing scopes completes the pull). The database scope is the
//! pull's subject and never degrades — its permission refusal (or
//! any non-permission failure at any scope) refuses the whole run,
//! naming its scope. The db-level credential the bearer token serves
//! (#99) is the audience: a hardened server's automation user reads
//! exactly the database it owns.
//!
//! # The rendering (the engine's own printer)
//!
//! Each statement's result arrives as a JSON object (the `/sql`
//! contract's serialization of the engine's own value — probed
//! 3.3.0, and the vendored `exec/operators/info/*.rs` agree: leaves
//! are the definitions' DDL strings; ROOT's `system`/`config`
//! sections carry numbers, arrays, and `NONE`). Pull maps the JSON
//! back onto the engine's value type and prints with the engine's
//! own `ToSql` — the printer dialect the interchange reader walks
//! (`exec.rs`'s `info_for` is the same call on the embedded side),
//! so the text is exactly the engine's INFO rendering: no
//! hand-rolled quoting, no hand-rolled escaping. The one invented
//! choice is deliberate and documented: JSON `null` maps to `NONE`
//! (the wire serialization does not distinguish `NONE` from `NULL`;
//! INFO's sections spell `NONE`, and the readers carry it as an
//! opaque token they never read). A number the engine's value type
//! cannot carry round-trips (a non-finite float) refuses — nothing
//! is invented for a shape the engine itself cannot spell.
//!
//! # The wire discipline
//!
//! The statements go as SEPARATE queries in ONE POST per leg — the
//! scopes leg (`INFO FOR ROOT; INFO FOR NS; INFO FOR DB;`), then the
//! tables leg (one `INFO FOR TABLE <name>;` per table, in the db
//! result's SORTED key order — the converted engine Value's BTreeMap
//! order; the wire's insertion order is lost at the conversion) once
//! the db element has named them; no
//! tables means no tables leg at all. The response array is
//! position-mapped to the queries and length-checked. The SCOPES leg
//! classifies its elements per scope (module doc, "The scopes leg"):
//! a permission refusal degrades the scope set, the db scope's own
//! refusal and any other kind refuse the run. The TABLES leg keeps
//! the strict walk — every element must be `status: "OK"` with an
//! object result; an ERR refuses with the engine's message
//! ([`TransportError::InfoRead`]; there is no fresh-database case
//! here — INFO always exists for a namespace/database that does).
//! Nothing but INFO is ever sent.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::DerefMut as _;
use std::path::{Path, PathBuf};

use surrealdb_types::{Array, Number, Object, ToSql as _, Value};

use super::json::Json;
use super::transport::{
    Target, TransportError, agent, element_message, element_permission_refused,
    element_result_object, info_elements, info_results,
};
use crate::info::normalize::TABLE_CHILDREN_KEY;
use crate::info::script::ident_text;

/// The pull's product: the fetched interchange files, the ready-made
/// `migrate generate` command that ingests the database scope, and
/// the advice printed under it. A scope the credential was refused
/// (issue #103) holds `None` and wrote no file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullOutcome {
    /// The `root=` interchange file — `None` when the credential was
    /// refused the root scope.
    pub root: Option<PathBuf>,
    /// The `ns=<ns>=` interchange file — `None` when the credential
    /// was refused the namespace scope.
    pub ns: Option<PathBuf>,
    /// The `db=<ns>/<db>=` interchange file (the `@tables` children
    /// assembled inside). Always present — the db scope is the pull's
    /// subject; its refusal refuses the run.
    pub db: PathBuf,
    /// How many tables the tables leg fetched (the db file's
    /// `@tables` map size).
    pub tables: usize,
    /// The printed command — the db-scoped `migrate generate`
    /// invocation (issue #100): exactly one `--info` tag, the
    /// database's, so the pasted happy path never compares a scope
    /// the schema file does not manage.
    pub command: String,
    /// The advice lines printed under the command: the fetched
    /// scopes' `--info` tags beyond the database (the wider compares
    /// are deliberate acts, never part of the pasted default), then
    /// the classification fact that scopes them (issue #100), then —
    /// for a degraded pull — the refused scopes' note (issue #103).
    pub wider_scopes: Vec<String>,
}

/// Everything pull refuses, with its user-facing text (the CLI
/// prints `error: {e}` and exits 1). No variant carries credential
/// bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PullError {
    /// The wire layer refused — the endpoint spelling, a header, the
    /// HTTP answers, the connection, an INFO read, or a response
    /// shape ([`TransportError`]'s own text rides verbatim).
    Wire(TransportError),
    /// A coordinate's name cannot ride the interchange file name —
    /// pull writes `surform-info-<scope>-<name>.txt`, and a path
    /// separator would nest (or escape) `--dir`; a `=` would make the
    /// printed `--info` tag (`ns=<name>=<path>`) un-parseable. Empty
    /// is refused with it (an empty `Surreal-NS`/`Surreal-DB` header
    /// names nothing on the server either).
    NameUnsafe { coordinate: String, value: String },
    /// An INFO result carries a shape the interchange cannot hold —
    /// no `tables` object in the db result, or a result the engine's
    /// own value type cannot carry back (module doc, "The
    /// rendering").
    InfoShape { detail: String },
    /// An interchange file could not be written.
    Unwritable { path: PathBuf, reason: String },
    /// A scopes-leg statement failed with a kind that is not the
    /// permission refusal — a real failure (a mistyped `--ns`/`--db`
    /// among them). Names the scope that refused: the pull cannot
    /// degrade on it, because the scope's INFO is simply absent
    /// (issue #103's self-explaining refusal).
    ScopeFailed { scope: String, message: String },
    /// The database scope itself rode the permission refusal. The db
    /// scope is the pull's subject — a pull without it is nothing —
    /// so unlike the root/ns refusals this refuses the whole run
    /// (issue #103).
    ScopeRefused { message: String },
}

impl fmt::Display for PullError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PullError::Wire(error) => write!(f, "{error}"),
            PullError::NameUnsafe { coordinate, value } if value.is_empty() => write!(
                f,
                "the {coordinate} name is empty — pass --{coordinate} (or set \
                 SURFORM_{} to a non-empty value; an empty variable counts as \
                 given and names nothing)",
                coordinate.to_ascii_uppercase()
            ),
            PullError::NameUnsafe { coordinate, value } if value.contains('=') => write!(
                f,
                "the {coordinate} name `{value}` carries `=` — the interchange \
                 tags split on it (`--info ns=<name>=<path>`), so no tag could \
                 name this scope's file; pull into a name without one"
            ),
            PullError::NameUnsafe { coordinate, value } if value.contains('\'') => write!(
                f,
                "the {coordinate} name `{value}` carries `'` — the printed next \
                 command single-quotes its paths, and no tag could survive it; \
                 pull into a name without one"
            ),
            PullError::NameUnsafe { coordinate, value } => write!(
                f,
                "the {coordinate} name `{value}` carries a path separator and \
                 cannot ride an interchange file name (pull writes \
                 `surform-info-{coordinate}-<name>.txt` into --dir) — pull into \
                 the database's real name"
            ),
            PullError::InfoShape { detail } => write!(
                f,
                "the database's INFO result carries a shape the interchange \
                 cannot hold — {detail}"
            ),
            PullError::Unwritable { path, reason } => {
                write!(f, "the file {} cannot be written: {reason}", path.display())
            }
            PullError::ScopeFailed { scope, message } => {
                write!(f, "the {scope} scope read failed - {message}")
            }
            PullError::ScopeRefused { message } => write!(
                f,
                "the database scope was refused for this credential - not \
                 permitted ({message}); a pull reads the db scope or does not \
                 run - pull with a credential that can read it"
            ),
        }
    }
}

impl From<TransportError> for PullError {
    fn from(error: TransportError) -> Self {
        PullError::Wire(error)
    }
}

/// The pull run: two legs over the shared transport, then the fetched
/// scopes' files, then the command. All fetches precede all writes
/// (module doc, "The scope").
pub fn pull(dir: &Path, target: &Target) -> Result<PullOutcome, PullError> {
    target.validate()?;
    refuse_unsafe_name("ns", &target.ns)?;
    refuse_unsafe_name("db", &target.db)?;
    let agent = agent();
    // Leg 1 — the three session scopes as separate queries in one
    // POST; the response array is position-mapped and the elements
    // classify per scope (issue #103): a permission refusal drops the
    // scope from the pull, any other failure refuses the run naming
    // its scope, and the db scope never degrades.
    let elements = info_elements(
        &agent,
        target,
        "INFO FOR ROOT; INFO FOR NS; INFO FOR DB;",
        3,
    )?;
    let root = scope_element(&elements[0], "root")?;
    let ns = scope_element(&elements[1], "ns")?;
    let mut db = scope_element(&elements[2], "db")?.ok_or_else(|| PullError::ScopeRefused {
        message: element_message(&elements[2]),
    })?;
    // Leg 2 — the tables the db result names, one INFO FOR TABLE per
    // query in its own key order; assembled under the reserved key.
    let table_names: Vec<String> = match db.get("tables") {
        Some(Value::Object(tables)) => tables.keys().cloned().collect(),
        _ => {
            return Err(PullError::InfoShape {
                detail: "the `INFO FOR DB` result carries no `tables` object".to_string(),
            });
        }
    };
    let children = if table_names.is_empty() {
        Object::new()
    } else {
        let queries: Vec<String> = table_names
            .iter()
            .map(|name| format!("INFO FOR TABLE {};", ident_text(name)))
            .collect();
        let results = info_results(&agent, target, &queries.join(" "), table_names.len())?;
        let mut children = Object::new();
        for (name, result) in table_names.iter().zip(&results) {
            let object = match json_to_value(result)? {
                Value::Object(object) => object,
                _ => {
                    return Err(PullError::InfoShape {
                        detail: format!("the `INFO FOR TABLE {name}` result is not an object"),
                    });
                }
            };
            BTreeMap::insert(&mut children, name.clone(), Value::Object(object));
        }
        children
    };
    BTreeMap::insert(
        &mut db,
        TABLE_CHILDREN_KEY.to_string(),
        Value::Object(children),
    );
    // The interchange text: the engine's own printer (module doc,
    // "The rendering"). A refused scope renders nothing and writes
    // no file.
    let root_text = root.map(|object| Value::Object(object).to_sql());
    let ns_text = ns.map(|object| Value::Object(object).to_sql());
    let db_text = Value::Object(db).to_sql();
    // Every fetch is done — the writes start here. At the default
    // directory the files spell bare (the printed command's shape);
    // an explicit --dir joins.
    let file_path = |name: &str| -> PathBuf {
        if dir == Path::new(".") {
            PathBuf::from(name)
        } else {
            dir.join(name)
        }
    };
    std::fs::create_dir_all(dir).map_err(|reason| PullError::Unwritable {
        path: dir.to_path_buf(),
        reason: reason.to_string(),
    })?;
    let root_path = file_path("surform-info-root.txt");
    let ns_path = file_path(&format!("surform-info-ns-{}.txt", target.ns));
    let db_path = file_path(&format!("surform-info-db-{}-{}.txt", target.ns, target.db));
    for (path, text) in [
        root_text.as_ref().map(|text| (&root_path, text)),
        ns_text.as_ref().map(|text| (&ns_path, text)),
        Some((&db_path, &db_text)),
    ]
    .into_iter()
    .flatten()
    {
        // The POSIX trailing newline — the reader skips trailing
        // trivia, and a file a shell prints should end with one.
        std::fs::write(path, format!("{text}\n")).map_err(|reason| PullError::Unwritable {
            path: path.clone(),
            reason: reason.to_string(),
        })?;
    }
    let command = print_command(&db_path, target);
    let wider_scopes = print_advice(
        root_text.as_ref().map(|_| root_path.as_path()),
        ns_text.as_ref().map(|_| ns_path.as_path()),
        target,
    );
    Ok(PullOutcome {
        root: root_text.as_ref().map(|_| root_path.clone()),
        ns: ns_text.as_ref().map(|_| ns_path.clone()),
        db: db_path,
        tables: table_names.len(),
        command,
        wider_scopes,
    })
}

/// The ready-made next command, database scope only (issue #100): one
/// `--info` tag, the database's. The wider scopes stay off the pasted
/// line on purpose — a schema file does not manage the server's root
/// users or the default config (the secrets rule keeps them out of
/// SDL), so a root-tagged compare classifies those statements as
/// removals; the happy path never carries that compare silently
/// ([`print_advice`] names the wider tags as deliberate acts).
/// The printed next command — the unit's headline product, so its
/// shell quoting is conservative (the one [`shell_safe`] rule): every
/// path/token rides SINGLE quotes (the only character needing care
/// inside them is `'`, which the `=`-family name refusals bar from
/// scope names and the `'\''` escape covers for file paths) — double
/// quotes would let `$`/backtick interpolate a server-chosen scope
/// name.
fn print_command(db: &Path, target: &Target) -> String {
    format!(
        "surform migrate generate schema.surdl --info {}",
        shell_safe(&format!("db={}/{}={}", target.ns, target.db, db.display())),
    )
}

/// The advice lines printed under the command: the fetched scopes'
/// tags beyond the database (named with their written paths, in the
/// same single-quoted shell spelling), then the classification fact
/// that scopes them — the removals live in the wider compares, never
/// in the pasted default — then, for a degraded pull, the refused
/// scopes' note (issue #103: no file was written for a refused
/// scope; a credential that can read it completes the pull).
/// Imperatives address the reader's own actions only (the repo's
/// voice contract). A full pull's lines are byte-identical to the
/// pre-#103 shape.
fn print_advice(root_path: Option<&Path>, ns_path: Option<&Path>, target: &Target) -> Vec<String> {
    let mut lines = Vec::new();
    let mut tags = Vec::new();
    if let Some(path) = root_path {
        tags.push(format!(
            "--info root={}",
            shell_safe(&path.display().to_string())
        ));
    }
    if let Some(path) = ns_path {
        tags.push(format!(
            "--info {}",
            shell_safe(&format!("ns={}={}", target.ns, path.display())),
        ));
    }
    if !tags.is_empty() {
        lines.push(format!(
            "the wider compares: add {} to the command above",
            tags.join(" and "),
        ));
        lines.push(
            "a scope the schema file does not manage classifies as removals (the \
             server's root users among them) - read the drift report before running \
             a wider compare's script"
                .to_string(),
        );
    }
    let refused: Vec<&str> = [
        ("root", root_path.is_none()),
        ("namespace", ns_path.is_none()),
    ]
    .into_iter()
    .filter_map(|(name, refused)| refused.then_some(name))
    .collect();
    match refused.as_slice() {
        [] => {}
        [one] => lines.push(format!(
            "the {one} scope was refused for this credential (not permitted) - no \
             file was written for it; a credential that can read it completes the \
             pull"
        )),
        [first, second] => lines.push(format!(
            "the {first} and {second} scopes were refused for this credential \
             (not permitted) - no files were written for them; a credential that \
             can read them completes the pull"
        )),
        _ => unreachable!("pull reads exactly three scopes; two can refuse"),
    }
    lines
}

/// One scopes-leg element's read: the scope's object, or `None` for
/// the engine's permission refusal (issue #103) — the caller owns the
/// policy (root/ns degrade, the db scope refuses). Any other ERR
/// refuses naming the scope; an OK element whose result is not an
/// object is an InfoShape refusal, as the strict walk's.
fn scope_element(element: &Json, scope: &str) -> Result<Option<Object>, PullError> {
    if element.get("status").and_then(Json::as_str) == Some("OK") {
        let result = element_result_object(element).map_err(PullError::Wire)?;
        match json_to_value(&result)? {
            Value::Object(object) => Ok(Some(object)),
            _ => Err(PullError::InfoShape {
                detail: format!(
                    "the `INFO FOR {}` result is not an object",
                    scope.to_ascii_uppercase()
                ),
            }),
        }
    } else if element_permission_refused(element) {
        Ok(None)
    } else {
        Err(PullError::ScopeFailed {
            scope: scope.to_string(),
            message: element_message(element),
        })
    }
}

/// The printed lines' one shell-quoting rule: every token rides SINGLE
/// quotes, with the `'\''` escape for an embedded `'` — the quoting
/// both [`print_command`] and [`print_wider_scopes`] print through.
fn shell_safe(piece: &str) -> String {
    format!("'{}'", piece.replace('\'', "'\\''"))
}

/// A coordinate's file-name and tag safety: no path separators (the
/// files would nest — or escape — `--dir`), no `=` (the `--info`
/// tags split on it), not empty (module doc on
/// [`PullError::NameUnsafe`]). Refused BEFORE any dial — a name the
/// files could not spell never reaches the wire.
fn refuse_unsafe_name(coordinate: &str, value: &str) -> Result<(), PullError> {
    if value.is_empty() || value.contains(['/', '\\', '=', '\'']) {
        return Err(PullError::NameUnsafe {
            coordinate: coordinate.to_string(),
            value: value.to_string(),
        });
    }
    Ok(())
}

/// The wire JSON's mapping onto the engine's own value type (module
/// doc, "The rendering"): object → object, string → string, number →
/// integer-or-float, boolean, array; JSON `null` → `NONE` (the wire
/// serialization does not distinguish it from `NULL`, and INFO's
/// sections spell `NONE`). A number neither spelling carries (beyond
/// the integer range, or non-finite as a float) refuses — the
/// engine's printer could not spell it back either.
fn json_to_value(value: &Json) -> Result<Value, PullError> {
    Ok(match value {
        Json::Null => Value::None,
        Json::Bool(b) => Value::Bool(*b),
        Json::Num(raw) => {
            if let Ok(int) = raw.parse::<i64>() {
                Value::Number(Number::Int(int))
            } else {
                match raw.parse::<f64>() {
                    Ok(float) if float.is_finite() => Value::Number(Number::Float(float)),
                    _ => {
                        return Err(PullError::InfoShape {
                            detail: format!("a number the printer cannot carry: {raw}"),
                        });
                    }
                }
            }
        }
        Json::Str(text) => Value::String(text.clone()),
        Json::Arr(items) => {
            let mut array = Array::new();
            for item in items {
                array.deref_mut().push(json_to_value(item)?);
            }
            Value::Array(array)
        }
        Json::Obj(members) => {
            let mut object = Object::new();
            for (key, value) in members {
                BTreeMap::insert(&mut object, key.clone(), json_to_value(value)?);
            }
            Value::Object(object)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rendering is the engine's own printer: a mapped object
    /// prints in the interchange's object-literal dialect — sorted
    /// keys, single-quoted strings, the reserved key double-quoted —
    /// and `read_info_object` walks it back to the same object.
    #[test]
    fn the_rendering_round_trips_through_the_interchange_reader() {
        let json = crate::migrate::json::parse(
            r#"{"tables": {"user": "DEFINE TABLE user TYPE NORMAL SCHEMALESS"},
                "namespaces": {}, "system": {"available_parallelism": 8, "load_average": [0.2, 1.0]},
                "config": null}"#,
        )
        .unwrap();
        let value = json_to_value(&json).unwrap();
        let text = value.to_sql();
        let walked = crate::info::read::read_info_object(&text)
            .unwrap_or_else(|diag| panic!("the rendering does not walk: {diag}\n{text}"));
        // The DDL string rides intact; the reserved key decodes; the
        // numbers/arrays/NONE ride as the readers' carried tokens.
        let Some(crate::info::read::InfoValue::Object(tables)) = walked.0.get("tables") else {
            panic!("no tables object in {text}");
        };
        assert!(
            matches!(
                tables.0.get("user"),
                Some(crate::info::read::InfoValue::Ddl(ddl))
                    if ddl == "DEFINE TABLE user TYPE NORMAL SCHEMALESS"
            ),
            "{text}"
        );
        assert!(text.contains("config: NONE"), "{text}");
        // The engine's own float spelling (SurrealQL's `f` suffix).
        assert!(text.contains("[0.2f, 1f]"), "{text}");
    }

    /// A number the engine's value type cannot carry round-trips
    /// refuses — nothing is invented (module doc, "The rendering").
    #[test]
    fn an_uncarryable_number_refuses() {
        let json = crate::migrate::json::parse("1e400").unwrap();
        let error = json_to_value(&json).unwrap_err();
        let PullError::InfoShape { detail } = error else {
            panic!("expected InfoShape");
        };
        assert!(detail.contains("1e400"), "{detail}");
    }

    /// The printed command's shape (issue #100): database scope only,
    /// every token riding SINGLE quotes — unconditional, so a
    /// server-chosen scope name can never interpolate through double
    /// quotes (`$` and backtick inside `"` execute;
    /// inside `'` nothing does).
    #[test]
    fn the_printed_command_single_quotes_every_token() {
        let target = Target {
            endpoint: "http://h".into(),
            ns: "acme".into(),
            db: "my shop".into(),
            auth: super::super::transport::Auth::None,
        };
        let command = print_command(Path::new("surform-info-db-acme-my shop.txt"), &target);
        assert_eq!(
            command,
            "surform migrate generate schema.surdl \
             --info 'db=acme/my shop=surform-info-db-acme-my shop.txt'"
        );
    }

    /// The advice lines for a full pull (issue #100): the two tags the
    /// db-scoped command leaves out, named with their written paths in
    /// the same single-quoted spelling, then the classification fact
    /// that scopes them — the removals live in the wider compares,
    /// never in the pasted default.
    #[test]
    fn the_advice_names_the_wider_tags_and_their_hazard() {
        let target = Target {
            endpoint: "http://h".into(),
            ns: "acme".into(),
            db: "shop".into(),
            auth: super::super::transport::Auth::None,
        };
        let lines = print_advice(
            Some(Path::new("surform-info-root.txt")),
            Some(Path::new("surform-info-ns-acme.txt")),
            &target,
        );
        assert_eq!(
            lines[0],
            "the wider compares: add --info root='surform-info-root.txt' and \
             --info 'ns=acme=surform-info-ns-acme.txt' to the command above"
        );
        let hazard = &lines[1];
        assert!(
            hazard.starts_with(
                "a scope the schema file does not manage \
                 classifies as removals"
            ),
            "{hazard}"
        );
        assert!(hazard.contains("read the drift report"), "{hazard}");
        assert_eq!(lines.len(), 2, "a full pull carries no refused note");
    }

    /// The advice for a degraded pull (issue #103): the fetched
    /// namespace's tag alone, the classification fact, then the
    /// refused root scope's note. The db-only pull's advice is the
    /// refused note alone — no wider tags exist to name. The matrix's
    /// fourth cell (root fetched, namespace refused) rounds the
    /// ladder out: the root tag alone, the singular "namespace" note.
    #[test]
    fn the_advice_names_a_degraded_pulls_refusals() {
        let target = Target {
            endpoint: "http://h".into(),
            ns: "acme".into(),
            db: "shop".into(),
            auth: super::super::transport::Auth::None,
        };
        let lines = print_advice(None, Some(Path::new("surform-info-ns-acme.txt")), &target);
        assert_eq!(
            lines[0],
            "the wider compares: add --info 'ns=acme=surform-info-ns-acme.txt' \
             to the command above"
        );
        let note = &lines[2];
        assert!(
            note.starts_with("the root scope was refused for this credential"),
            "{note}"
        );
        assert!(note.contains("completes the pull"), "{note}");
        let lines = print_advice(None, None, &target);
        assert_eq!(
            lines[0],
            "the root and namespace scopes were refused for this credential \
             (not permitted) - no files were written for them; a credential \
             that can read them completes the pull"
        );
        assert_eq!(lines.len(), 1, "{lines:?}");
        let lines = print_advice(Some(Path::new("surform-info-root.txt")), None, &target);
        assert_eq!(
            lines[0],
            "the wider compares: add --info root='surform-info-root.txt' \
             to the command above"
        );
        let note = &lines[2];
        assert!(
            note.starts_with("the namespace scope was refused for this credential"),
            "{note}"
        );
        assert!(note.contains("completes the pull"), "{note}");
    }
}
