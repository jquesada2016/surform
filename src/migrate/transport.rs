//! The `POST /sql` transport — the pipeline's one connectivity
//! surface (the client, extracted from `push`; status, revert, and
//! pull ride the same target, the same
//! POST, the same response walk, and the same wire-error family as
//! the apply loop — one endpoint, one credential discipline, one
//! classification substrate).
//!
//! # The wire contract (doc-cited; never assumed)
//!
//! Every claim below cites its page — the two sources are
//! <https://surrealdb.com/docs/reference/rest-api/http-protocol.md>
//! (the `POST /sql` section) and
//! <https://surrealdb.com/docs/reference/rest-api/errors.md> (the
//! error shapes). Where the docs were silent, the behavior was
//! engine-probed against a real 3.3.0 server and the probe is named.
//!
//! - **Request**: `POST <server-root>/sql`. The body is raw
//!   SurrealQL — "a set of SurrealQL statements" — multiple
//!   `;`-separated statements in ONE body (http-protocol.md, `POST
//!   /sql`: "This HTTP endpoint expects the HTTP body to be a set of
//!   SurrealQL statements"; every example sends two statements in one
//!   `-d`). Bodies over 1 MiB are refused by the server with 413 —
//!   far above any migration file; nothing here chunks.
//! - **Headers** (http-protocol.md `POST /sql` Headers table):
//!   `Accept: application/json` (required — "sets the desired
//!   content-type of the response"; the spelling of every example),
//!   `Surreal-NS` and `Surreal-DB` (required — the namespace and
//!   database the statements run in), `Authorization` (optional —
//!   "sets the root, namespace, database, or record authentication
//!   data"). Basic auth rides `Authorization: Basic
//!   base64(user:pass)` — the docs' `-u "root:secret"` curl spelling,
//!   which the page notes "is a shorthand used by curl to send an
//!   Authorization header". A Bearer token rides `Authorization:
//!   Bearer <token>` (probed 3.3.0: accepted, HTTP 200). The docs'
//!   V2.x example spelling a LITERAL `Bearer:` header is stale on
//!   3.3: probed, it authenticates nothing (the request lands
//!   anonymous, 403). This pipeline speaks exactly one credential per
//!   run — Basic (`--user`/`--pass` or `SURFORM_DB_USER`/
//!   `SURFORM_DB_PASS`), a bearer token (`--token` or
//!   `SURFORM_DB_TOKEN` — the caller mints it, one `POST /signin` with
//!   the db-level credential, and hands it over; the pipeline sends it
//!   verbatim and never mints or refreshes), or no-auth for servers
//!   run with `--unauthenticated` (issue #99 added the token; the
//!   signin-minting mode was declined by owner ruling).
//! - **Response** (`Accept: application/json`): HTTP 200 with a JSON
//!   ARRAY, one element per STATEMENT (probed: leading `--` comment
//!   lines — a committed file's header — produce NO elements). Each
//!   element carries `time` (string), `status` (`"OK"` or `"ERR"`),
//!   `result` (the statement's result; on ERR, the error MESSAGE as a
//!   string), and `type` (http-protocol.md's response example;
//!   errors.md's statement-level examples).
//! - **Two failure layers** (errors.md, "Over HTTP"): the REQUEST can
//!   fail as a whole — then the HTTP status carries it: 400 parse
//!   error (probed: a JSON body `{code, details, description,
//!   information}`), 401 credentials that do not authenticate (probed:
//!   the body is PLAIN TEXT — "The password did not verify" — not
//!   JSON), 403 anonymous/capability-refused (probed: JSON error
//!   object), 413 body over the cap, 504 query timeout (since 3.3.0).
//!   Once the request is ACCEPTED the response is 200 "even when a
//!   statement fails. … Each failure is reported in that statement's
//!   entry in the response array, with `status` set to `"ERR"`", so
//!   — errors.md verbatim — "Code that calls these endpoints must
//!   therefore check `status` on each entry as well as the HTTP
//!   status." That check IS [`judge`].
//! - **Transactions** (errors.md, "Inside a transaction"; probed
//!   3.3.0): a statement that fails inside `BEGIN` … `COMMIT` aborts
//!   the transaction — the docs' example and the probe agree nothing
//!   inside survives. The probe sharpened the element shapes: on
//!   abort, statements that HAD succeeded inside the transaction
//!   report `status: "ERR"`, `kind: "Query"`,
//!   `details.kind: "NotExecuted"` ("The query was not executed due
//!   to a failed transaction") — NOT their original OK — later
//!   statements report `details.kind: "Cancelled"`, the `COMMIT`
//!   reports "Cannot COMMIT: the transaction was aborted due to a
//!   prior error", and the ROOT CAUSE carries its own kind
//!   (`AlreadyExists` in the probe) at its own array position, which
//!   can come AFTER `NotExecuted` elements. Hence [`judge`]'s
//!   root-cause rule: the first ERR element whose kind/details-kind is
//!   neither `NotExecuted` nor `Cancelled`, falling back to the first
//!   ERR. The success predicate is unaffected for the apply loop: the
//!   whole file is one transaction, every abort surfaces at the
//!   COMMIT element, so all-OK ⇔ applied.
//! - **The ledger read**: `SELECT number, name, hash FROM <table>;`
//!   rides the same endpoint and parses like any query. Probed 3.3.0:
//!   a row is an object `{number: <json number>, name: <string>,
//!   hash: <string>}`; an empty table yields `result: []`; a MISSING
//!   table is a per-statement ERR with HTTP 200 and `kind: "NotFound"`,
//!   `details.kind: "Table"` — the fresh-database case, matched on the
//!   kind AND the details.kind SUBJECT (the errors page's sanctioned
//!   branch keys, so no message text is matched — messages "are free
//!   to change between releases"; kinds are "a stable contract").
//!   NotFound's subject list covers more than tables (Namespace,
//!   Database, Record, Session, Transaction, Method): a missing
//!   NAMESPACE or DATABASE — a mistyped `--ns`/`--db` — reports the
//!   same top-level kind, and is a real error here, never an empty
//!   ledger; any other kind refuses likewise.
//! - **The permission verdict** (issue #103): [`element_permission_refused`]
//!   reads the top-level `kind == "NotAllowed"` off the wire element
//!   — the composition is the errors page's sanctioned one, and the
//!   shapes behind it were captured from BOTH sides: `details.kind`
//!   probed LIVE for the other error kinds (the bullets above), the
//!   engine's NotAllowed shape probed IN-PROCESS
//!   (`tests/engine_scope_permissions.rs` — details.kind `Auth`, the
//!   IAM verdict). A refusal whose kind is anything else is NOT a
//!   permission verdict: pull's scopes walk fails closed on it
//!   (`PullError::ScopeFailed`, the run refuses).

use std::fmt;
use std::time::Duration;

use super::json::Json;

/// The connect timeout — the only timeout the pipeline sets. Reads
/// are UNBOUNDED on purpose: a migration's data slots can
/// legitimately run long, and killing one mid-flight would be worse
/// than waiting. Every command's agent rides the same config.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The agent every command dials through — one config (the connect
/// timeout, HTTP statuses NOT surfaced as ureq errors: the wire layer
/// maps them itself, the body riding along).
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// How the pipeline authenticates to the endpoint — exactly one
/// credential per run (the CLI refuses a token alongside a user or
/// password). Credentials ride flags or environment variables (the CLI
/// resolves which) and NEVER enter a log, an error message, or a
/// report — the header is built in [`post_sql`] and nothing else sees
/// the bytes. `Debug` is manual for the same reason: it prints the
/// VARIANT, never the fields, so a future `{:?}` cannot leak the
/// secret.
#[derive(Clone, Default, PartialEq, Eq)]
pub enum Auth {
    /// No `Authorization` header — for servers run with
    /// `--unauthenticated` (http-protocol.md's Setup section).
    #[default]
    None,
    /// `Authorization: Basic base64(user:pass)` — the docs' `-u`
    /// spelling (http-protocol.md, `POST /sql` Example usage).
    Basic { user: String, pass: String },
    /// `Authorization: Bearer <token>` (probed 3.3.0: accepted, HTTP
    /// 200) — a JWT the caller minted (`POST /signin` with the
    /// db-level credential) and handed over; the pipeline sends it
    /// verbatim and never mints or refreshes it.
    Token { token: String },
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Auth::None => f.write_str("Auth::None"),
            Auth::Basic { .. } => f.write_str("Auth::Basic { .. }"),
            Auth::Token { .. } => f.write_str("Auth::Token { .. }"),
        }
    }
}

/// Where a command applies to: the server root plus the session scope
/// the `/sql` headers carry. `Debug` shows the connection coordinates
/// and the (self-redacting) [`Auth`] — never a credential byte.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Target {
    /// The server ROOT (`http://host:8000`) — `/sql` is appended here,
    /// never typed by the user.
    pub endpoint: String,
    /// The `Surreal-NS` header (required — http-protocol.md).
    pub ns: String,
    /// The `Surreal-DB` header (required — http-protocol.md).
    pub db: String,
    pub auth: Auth,
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target")
            .field("endpoint", &self.endpoint)
            .field("ns", &self.ns)
            .field("db", &self.db)
            .field("auth", &self.auth)
            .finish()
    }
}

impl Target {
    /// The connection surface's pre-flight: the endpoint may carry no
    /// surrounding whitespace (a padded root would pass the scheme
    /// check below and die in the transport layer on the untrimmed
    /// URI — an off-point message; named up front instead),
    /// the scheme must be http(s)
    /// (all this tool's endpoints are), the path must not already end
    /// in `/sql` (the suffix is appended here — a doubled one would
    /// 404 confusingly) nor carry `?`/`#` (a query or fragment would
    /// corrupt the appended `/sql` path — `url()` appends AFTER them),
    /// and no header VALUE may carry a control character (the HTTP
    /// layer would refuse it with the value in the error text — the
    /// one redaction hole this closes up front; the refusal names the
    /// header, never its value).
    pub fn validate(&self) -> Result<(), TransportError> {
        let root = self.endpoint.trim();
        if root.len() != self.endpoint.len() {
            return Err(TransportError::EndpointInvalid {
                endpoint: self.endpoint.clone(),
                reason: "the endpoint carries leading or trailing whitespace — pass the bare root"
                    .to_string(),
            });
        }
        if !(root.starts_with("http://") || root.starts_with("https://")) {
            return Err(TransportError::EndpointInvalid {
                endpoint: self.endpoint.clone(),
                reason: "the endpoint must start with `http://` or `https://`".to_string(),
            });
        }
        if root.trim_end_matches('/').ends_with("/sql") {
            return Err(TransportError::EndpointInvalid {
                endpoint: self.endpoint.clone(),
                reason: "pass the server root — `/sql` is appended automatically".to_string(),
            });
        }
        if root.contains(['?', '#']) {
            return Err(TransportError::EndpointInvalid {
                endpoint: self.endpoint.clone(),
                reason: "pass the bare server root — a `?` or `#` would corrupt the \
                         appended `/sql` path"
                    .to_string(),
            });
        }
        header_value("Surreal-NS", &self.ns)?;
        header_value("Surreal-DB", &self.db)?;
        match &self.auth {
            Auth::Basic { user, pass } => {
                header_value("Authorization user", user)?;
                header_value("Authorization password", pass)?;
            }
            Auth::Token { token } => header_value("Authorization token", token)?,
            Auth::None => {}
        }
        Ok(())
    }

    /// The full `/sql` URL (the root's trailing slashes trimmed).
    /// `validate` refuses surrounding whitespace before any request,
    /// so the spelling here is the one the `/sql`-suffix check tested.
    fn url(&self) -> String {
        format!("{}/sql", self.endpoint.trim_end_matches('/'))
    }
}

/// A header value's redaction guard: control characters (which would
/// make the HTTP layer reject the request, naming the value) refuse
/// up front, naming the HEADER only.
fn header_value(header: &str, value: &str) -> Result<(), TransportError> {
    if value.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return Err(TransportError::HeaderValueInvalid {
            header: header.to_string(),
        });
    }
    Ok(())
}

/// One `/sql` round trip's outcome: the HTTP status and the raw body.
/// The `http` crate types behind the response never leave this
/// module. Shared by every command (one data path — the same client,
/// the same transport mapping).
pub(crate) struct SqlOutcome {
    pub(crate) status: u16,
    pub(crate) body: String,
}

/// The credential's `Authorization` header VALUE — `None` for no
/// credential (the header is simply absent, the anonymous form). One
/// home for the two spellings: Basic's `base64(user:pass)` (the docs'
/// `-u` shorthand) and the bearer token's verbatim ride. The value
/// feeds [`post_sql`]'s header alone; nothing formats it anywhere
/// else.
fn authorization_header(auth: &Auth) -> Option<String> {
    match auth {
        Auth::None => None,
        Auth::Basic { user, pass } => {
            let secret = super::base64::encode(format!("{user}:{pass}").as_bytes());
            Some(format!("Basic {secret}"))
        }
        Auth::Token { token } => Some(format!("Bearer {token}")),
    }
}

/// The one HTTP call the pipeline makes: POST the body to
/// `<root>/sql` with the contract's headers, returning the status and
/// body. Transport failures (connection refused, DNS, TLS) map to
/// [`TransportError::Transport`]; the URL never carries credentials
/// (auth rides the header), so no error path can echo them.
pub(crate) fn post_sql(
    agent: &ureq::Agent,
    target: &Target,
    body: &str,
) -> Result<SqlOutcome, TransportError> {
    let mut request = agent
        .post(target.url())
        .header("Accept", "application/json")
        .header("Surreal-NS", &target.ns)
        .header("Surreal-DB", &target.db);
    if let Some(value) = authorization_header(&target.auth) {
        request = request.header("Authorization", &value);
    }
    // The endpoint rides the error (issue #90: the refusal names the
    // URL it could not reach — the `/sql` surface, not the bare root).
    let url = target.url();
    let mut response = request
        .send(body.as_bytes())
        .map_err(|error| transport_error(error, &url))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| transport_error(error, &url))?;
    Ok(SqlOutcome { status, body: text })
}

/// ureq's transport-layer errors (io, TLS, protocol, URI) — their
/// text is connection-level (addresses, socket reasons) and carries
/// no credential bytes (auth rides a header this module builds and
/// never formats).
fn transport_error(error: ureq::Error, endpoint: &str) -> TransportError {
    TransportError::Transport {
        endpoint: endpoint.to_string(),
        reason: error.to_string(),
    }
}

/// The wire layer's refusals — the connection, the HTTP answers, and
/// the response shapes. No variant carries credential bytes. The CLI
/// prints `error: {e}` and exits 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// The endpoint is not a server root this tool can post to.
    EndpointInvalid { endpoint: String, reason: String },
    /// A header value (namespace, database, or a credential) carries a
    /// control character. The VALUE is never named.
    HeaderValueInvalid { header: String },
    /// The server answered at the HTTP layer (auth, parse, size) —
    /// the status and its body verbatim (server text; carries no
    /// credential bytes).
    Http { status: u16, body: String },
    /// The connection itself failed (transport text; no credentials
    /// ride transport errors). The endpoint is named — a refusal that
    /// says only "the connection failed" leaves the operator guessing
    /// WHICH server (issue #90: connection failures name the
    /// endpoint).
    Transport { endpoint: String, reason: String },
    /// The ledger SELECT failed with a kind other than NotFound — a
    /// real database error, not the fresh-database case.
    LedgerRead { message: String },
    /// A schema read (`INFO FOR DB` / `INFO FOR TABLE` — the revert
    /// and pull paths) failed at the statement level: the engine's
    /// message rides. There is no fresh-database case here — an INFO
    /// on a namespace or database that does not exist (a mistyped
    /// `--ns`/`--db`) is exactly this refusal, never an empty schema.
    InfoRead { message: String },
    /// The response did not spell the documented shapes.
    MalformedResponse { detail: String },
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::EndpointInvalid { endpoint, reason } => {
                write!(f, "the endpoint {endpoint} is not usable — {reason}")
            }
            TransportError::HeaderValueInvalid { header } => write!(
                f,
                "the {header} value carries a control character and cannot ride an \
                 HTTP header"
            ),
            TransportError::Http { status, body } => {
                write!(f, "the server answered HTTP {status} — {}", body.trim())
            }
            TransportError::Transport { endpoint, reason } => {
                write!(f, "cannot reach {endpoint} — {reason}; is the server up?")
            }
            TransportError::LedgerRead { message } => {
                write!(f, "reading the migration ledger failed — {message}")
            }
            TransportError::InfoRead { message } => {
                write!(f, "reading the database schema failed — {message}")
            }
            TransportError::MalformedResponse { detail } => {
                write!(f, "the server's /sql response is malformed — {detail}")
            }
        }
    }
}

/// One ledger row — what skip-applied matches on. The projection is
/// deliberate: the `sdl` column (revert's substrate, often large)
/// stays off the wire until a revert asks for it — status and push
/// read the narrow row, revert reads the wider one
/// ([`read_ledger_rows`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerRow {
    pub(crate) name: String,
    pub(crate) hash: String,
}

/// A ledger SELECT's answer, parsed against the probed shapes
/// (module doc, "The ledger read"). The FRESH-DATABASE case is a
/// missing TABLE — `kind: "NotFound"` with `details.kind: "Table"`,
/// both from the errors page's stable contract (no message text is
/// matched). NotFound's other subjects — a missing namespace or
/// database (a mistyped `--ns`/`--db`; probed: both report the same
/// top-level kind) among them — and every other kind refuse with the
/// engine's message: an empty ledger is never a guess.
///
/// Shared by push, status, and revert (one data path): the response
/// walk, the fresh-database branch, and the number parse are ONE
/// surface; only the row projection differs (push/status read
/// name+hash, revert adds the `sdl` column).
pub(crate) fn read_ledger_rows<T>(
    outcome: SqlOutcome,
    project: impl Fn(&Json) -> Result<T, TransportError>,
) -> Result<std::collections::BTreeMap<u32, T>, TransportError> {
    if outcome.status != 200 {
        return Err(TransportError::Http {
            status: outcome.status,
            body: outcome.body,
        });
    }
    let document = json_document(&outcome.body)?;
    let elements = element_array(&document)?;
    let first = elements
        .first()
        .ok_or_else(|| TransportError::MalformedResponse {
            detail: "the response array is empty".to_string(),
        })?;
    let Some(status) = first.get("status").and_then(Json::as_str) else {
        return Err(TransportError::MalformedResponse {
            detail: "the response element carries no `status` string".to_string(),
        });
    };
    if status != "OK" {
        let fresh_database = element_kind(first).as_deref() == Some("NotFound")
            && element_reason(first).as_deref() == Some("Table");
        return match fresh_database {
            true => Ok(std::collections::BTreeMap::new()),
            false => Err(TransportError::LedgerRead {
                message: element_message(first),
            }),
        };
    }
    let rows = first
        .get("result")
        .and_then(Json::as_array)
        .ok_or_else(|| TransportError::MalformedResponse {
            detail: "the ledger SELECT's result is not an array".to_string(),
        })?;
    let mut ledger = std::collections::BTreeMap::new();
    for row in rows {
        let number = row
            .get("number")
            .and_then(Json::as_integer)
            .filter(|n| (0..=u32::MAX as i64).contains(n))
            .ok_or_else(|| TransportError::MalformedResponse {
                detail: "a ledger row carries no `number` integer".to_string(),
            })? as u32;
        ledger.insert(number, project(row)?);
    }
    Ok(ledger)
}

/// The narrow projection push and status read: the row's name and
/// hash. (`SELECT number, name, hash FROM <t>;` — the sdl column
/// never rides this query.)
pub(crate) fn read_ledger(
    outcome: SqlOutcome,
) -> Result<std::collections::BTreeMap<u32, LedgerRow>, TransportError> {
    read_ledger_rows(outcome, |row| {
        let parse_field = |key: &str| -> Result<String, TransportError> {
            row.get(key)
                .and_then(Json::as_str)
                .map(str::to_string)
                .ok_or_else(|| TransportError::MalformedResponse {
                    detail: format!("a ledger row carries no `{key}` string"),
                })
        };
        Ok(LedgerRow {
            name: parse_field("name")?,
            hash: parse_field("hash")?,
        })
    })
}

/// One INFO leg's answer over the wire — the response walk the INFO
/// readers share (pull's scopes and tables legs; revert's live
/// schema fetch). The leg's statements ride as separate queries in
/// ONE POST body; the response array is position-mapped to the
/// queries, LENGTH-CHECKED (the position mapping is never a guess),
/// and every element must be `status: "OK"` with an OBJECT `result`
/// (an INFO's result is always an object; anything else — a null
/// would render `NONE` and stall the readers — is refused here with
/// the precise reason, never invented). There is no fresh-database
/// case: an INFO on a namespace or database that does not exist is
/// the engine's NotFound and refuses
/// ([`TransportError::InfoRead`]) with the engine's message.
pub(crate) fn info_results(
    agent: &ureq::Agent,
    target: &Target,
    body: &str,
    queries: usize,
) -> Result<Vec<Json>, TransportError> {
    let elements = info_elements(agent, target, body, queries)?;
    let mut results = Vec::with_capacity(queries);
    for element in &elements {
        if element.get("status").and_then(Json::as_str) != Some("OK") {
            return Err(TransportError::InfoRead {
                message: element_message(element),
            });
        }
        results.push(element_result_object(element)?);
    }
    Ok(results)
}

/// The raw element array behind [`info_results`]: the same POST, the
/// same position mapping and length check — but the elements ride
/// home WITHOUT the all-OK enforcement, so a caller can classify ERR
/// elements itself (pull's scopes leg reads the permission verdicts;
/// issue #103). Everything else about the contract is unchanged.
pub(crate) fn info_elements(
    agent: &ureq::Agent,
    target: &Target,
    body: &str,
    queries: usize,
) -> Result<Vec<Json>, TransportError> {
    let outcome = post_sql(agent, target, body)?;
    if outcome.status != 200 {
        return Err(TransportError::Http {
            status: outcome.status,
            body: outcome.body,
        });
    }
    let document = json_document(&outcome.body)?;
    let elements = element_array(&document)?;
    if elements.len() != queries {
        return Err(TransportError::MalformedResponse {
            detail: format!(
                "the response carries {} element(s) for {queries} INFO statement(s)",
                elements.len()
            ),
        });
    }
    Ok(elements.to_vec())
}

/// An OK element's OBJECT `result` — the shape half of the
/// [`info_results`] walk, shared with pull's per-scope reader (an
/// INFO's result is always an object; anything else is refused with
/// the precise reason, never invented).
pub(crate) fn element_result_object(element: &Json) -> Result<Json, TransportError> {
    match element.get("result") {
        Some(result @ Json::Obj(_)) => Ok(result.clone()),
        Some(_) => Err(TransportError::MalformedResponse {
            detail: "an INFO statement's result is not a JSON object".to_string(),
        }),
        None => Err(TransportError::MalformedResponse {
            detail: "the response element carries no `result`".to_string(),
        }),
    }
}

/// The element's error KIND — the top-level category ("NotFound",
/// "AlreadyExists", "Query", …; errors.md: "branch on `kind` first").
/// The nested `details.kind` is the reason WITHIN the kind — a
/// missing table is `kind: "NotFound"` with `details.kind: "Table"`
/// (the thing not found), while a transaction's consequences are
/// `kind: "Query"` with `details.kind: "NotExecuted"/"Cancelled"`
/// (the reason). [`element_reason`] reads that nested spelling.
fn element_kind(element: &Json) -> Option<String> {
    element
        .get("kind")
        .and_then(Json::as_str)
        .map(str::to_string)
}

/// Whether an ERR element is the engine's PERMISSION verdict — the
/// stable top-level kind `NotAllowed` (errors.md: "branch on `kind`
/// first"; the types crate spells the variant "Permission or
/// authorization error"). Probe-pinned in-process against the
/// embedded 3.3.0 engine (`tests/engine_scope_permissions.rs`): a
/// db-level credential's `INFO FOR ROOT`/`INFO FOR NS` answer exactly
/// this kind (details.kind `Auth`, "IAM error: Not enough permissions
/// to perform this action") while its `INFO FOR DB` is OK, and an
/// ns-level credential's refusal stops one scope shorter — the shape
/// pull's scopes leg degrades on (issue #103). Message text is never
/// matched.
pub(crate) fn element_permission_refused(element: &Json) -> bool {
    element_kind(element).as_deref() == Some("NotAllowed")
}

/// The element's reason: `details.kind` when the nested shape rides,
/// the top-level kind otherwise.
pub(crate) fn element_reason(element: &Json) -> Option<String> {
    element
        .get("details")
        .and_then(|details| details.get("kind"))
        .and_then(Json::as_str)
        .or_else(|| element.get("kind").and_then(Json::as_str))
        .map(str::to_string)
}

/// The element's human-facing message: the `result` string — errors.md's
/// statement-level shape puts the message there — or, when the result
/// is not text, the element's own shape (a server the contract
/// outgrew; text beats silence).
pub(crate) fn element_message(element: &Json) -> String {
    match element.get("result").and_then(Json::as_str) {
        Some(message) => message.to_string(),
        None => format!("{element:?}"),
    }
}

pub(crate) fn json_document(body: &str) -> Result<Json, TransportError> {
    crate::migrate::json::parse(body).map_err(|error| TransportError::MalformedResponse {
        detail: error.to_string(),
    })
}

pub(crate) fn element_array(document: &Json) -> Result<&[Json], TransportError> {
    document
        .as_array()
        .ok_or_else(|| TransportError::MalformedResponse {
            detail: "the response is not a JSON array".to_string(),
        })
}

/// A statement-result array's verdict: `Ok(())` when every element is
/// `status: "OK"` (all-OK ⇔ the transaction committed — the whole
/// migration file is one `BEGIN`…`COMMIT` and every abort reports at
/// the COMMIT element, module doc "Transactions"), or the failure's
/// reason: the first ERR element whose kind is neither `NotExecuted`
/// nor `Cancelled` (those are the abort's CONSEQUENCES — the root
/// cause carries its own kind, probed; module doc), falling back to
/// the first ERR. A non-200 answer is the request-level failure layer
/// (errors.md "Over HTTP"): the body rides the reason whatever its
/// spelling (the 401 body is plain text, the 400 body JSON — probed).
pub(crate) fn judge(outcome: SqlOutcome) -> Result<(), String> {
    if outcome.status != 200 {
        return Err(format!(
            "the server answered HTTP {} — {}",
            outcome.status,
            outcome.body.trim()
        ));
    }
    let document = json_document(&outcome.body).map_err(|error| error.to_string())?;
    let elements = element_array(&document).map_err(|error| error.to_string())?;
    let failed = |element: &Json| element.get("status").and_then(Json::as_str) != Some("OK");
    if !elements.iter().any(failed) {
        return Ok(());
    }
    let first_error = elements.iter().find(|element| failed(element));
    let root = elements
        .iter()
        .find(|element| {
            failed(element)
                && !matches!(
                    element_reason(element).as_deref(),
                    Some("NotExecuted" | "Cancelled")
                )
        })
        .or(first_error);
    Err(root
        .map(element_message)
        .unwrap_or_else(|| "unknown failure".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe shapes' local spellings (the mock suite carries its
    /// own; these stay byte-identical to the 3.3.0 captures).
    const OK_ELEMENT: &str = r#"{"result":null,"status":"OK","time":"1.000ms","type":null}"#;

    fn err_element(kind: &str, details_kind: Option<&str>, message: &str) -> String {
        let details = match details_kind {
            Some(nested) => format!(r#""details":{{"kind":"{nested}"}},"#),
            None => String::new(),
        };
        format!(
            r#"{{{details}"kind":"{kind}","result":"{message}","status":"ERR","time":"1.000ms","type":null}}"#
        )
    }

    /// The verdict's success side: every element OK — the committed
    /// file's whole-transaction shape.
    #[test]
    fn judge_accepts_the_all_ok_array() {
        let outcome = SqlOutcome {
            status: 200,
            body: format!("[{OK_ELEMENT},{OK_ELEMENT},{OK_ELEMENT}]"),
        };
        assert_eq!(judge(outcome), Ok(()));
    }

    /// The 3.3.0 probe's transaction-abort array: the root cause
    /// (AlreadyExists) rides AFTER the NotExecuted consequences — the
    /// verdict carries the CAUSE's message, not the consequences'.
    #[test]
    fn judge_finds_the_root_cause_past_the_not_executed() {
        let abort = format!(
            "[{ok},{not_executed},{cause},{cancelled},{commit}]",
            ok = OK_ELEMENT,
            not_executed = err_element(
                "Query",
                Some("NotExecuted"),
                "The query was not executed due to a failed transaction",
            ),
            cause = err_element(
                "AlreadyExists",
                Some("Table"),
                "The table 't_probe' already exists",
            ),
            cancelled = err_element(
                "Query",
                Some("Cancelled"),
                "The query was not executed due to a cancelled transaction",
            ),
            commit = err_element(
                "Query",
                Some("NotExecuted"),
                "Cannot COMMIT: the transaction was aborted due to a prior error",
            ),
        );
        let outcome = SqlOutcome {
            status: 200,
            body: abort,
        };
        assert_eq!(
            judge(outcome),
            Err("The table 't_probe' already exists".to_string())
        );
    }

    /// The request-level layer: a non-200 answer — the probe's
    /// plain-text 401 — rides the body verbatim.
    #[test]
    fn judge_surfaces_the_http_layer_verbatim() {
        let outcome = SqlOutcome {
            status: 401,
            body: "The password did not verify".to_string(),
        };
        assert_eq!(
            judge(outcome),
            Err("the server answered HTTP 401 — The password did not verify".to_string())
        );
    }

    /// The kind/reason split: a missing table is kind NotFound with
    /// details.kind Table (the thing not found); a consequence is
    /// kind Query with details.kind NotExecuted (the reason).
    #[test]
    fn element_kinds_split_from_their_reasons() {
        let missing: Json = crate::migrate::json::parse(
            r#"{"details":{"details":{"name":"t"},"kind":"Table"},"kind":"NotFound","result":"The table 't' does not exist","status":"ERR","time":"1ms","type":null}"#,
        )
        .unwrap();
        assert_eq!(element_kind(&missing).as_deref(), Some("NotFound"));
        assert_eq!(element_reason(&missing).as_deref(), Some("Table"));
        let consequence: Json = crate::migrate::json::parse(
            r#"{"details":{"kind":"NotExecuted"},"kind":"Query","result":"not executed","status":"ERR","time":"1ms","type":null}"#,
        )
        .unwrap();
        assert_eq!(element_kind(&consequence).as_deref(), Some("Query"));
        assert_eq!(element_reason(&consequence).as_deref(), Some("NotExecuted"));
    }

    /// The fresh-database branch reads ONLY the missing-TABLE subject:
    /// kind NotFound with details.kind Table is the empty ledger,
    /// while NotFound on any other subject — a mistyped --ns/--db
    /// lands Namespace or Database (probed: both report the same
    /// top-level kind) — and a NotFound with no details at all refuse
    /// with the engine's message, never an empty-ledger guess.
    #[test]
    fn the_fresh_database_branch_is_the_table_subject_only() {
        let ledger_of = |element: &str| {
            read_ledger(SqlOutcome {
                status: 200,
                body: format!("[{element}]"),
            })
        };
        // The probed missing-table shape: an empty ledger.
        let missing = r#"{"details":{"details":{"name":"_surform_migrations"},"kind":"Table"},"kind":"NotFound","result":"The table '_surform_migrations' does not exist","status":"ERR","time":"1.000ms","type":null}"#;
        assert_eq!(
            ledger_of(missing).unwrap(),
            std::collections::BTreeMap::new()
        );
        // The wrong-namespace and wrong-database spellings (the probe's
        // shapes): real errors carrying the engine's message.
        for subject in ["Namespace", "Database"] {
            let element = err_element(
                "NotFound",
                Some(subject),
                &format!(
                    "The {subject_word} 'does-not-exist' does not exist",
                    subject_word = subject.to_ascii_lowercase()
                ),
            );
            let error = ledger_of(&element).expect_err(subject);
            let TransportError::LedgerRead { message } = error else {
                panic!("expected LedgerRead for {subject}");
            };
            assert!(message.contains("does not exist"), "{message}");
        }
        // No details: the reason falls back to the top-level kind —
        // not "Table", so it refuses (fail-closed).
        let bare =
            r#"{"kind":"NotFound","result":"not found","status":"ERR","time":"1ms","type":null}"#;
        assert!(matches!(
            ledger_of(bare),
            Err(TransportError::LedgerRead { .. })
        ));
    }

    /// The manual Debug impls keep credential bytes out of any `{:?}`
    /// (the derive printed the fields — the latent
    /// one-`{:?}`-from-leaking trap).
    #[test]
    fn debug_redacts_the_credentials() {
        let auth = Auth::Basic {
            user: "pushuser".to_string(),
            pass: "pushpass-secret".to_string(),
        };
        assert_eq!(format!("{auth:?}"), "Auth::Basic { .. }");
        let target = Target {
            endpoint: "http://h".into(),
            ns: "test".into(),
            db: "scratch".into(),
            auth,
        };
        let text = format!("{target:?}");
        assert!(text.contains("http://h"), "{text}");
        assert!(text.contains("Auth::Basic { .. }"), "{text}");
        assert!(!text.contains("pushuser"), "{text}");
        assert!(!text.contains("pushpass-secret"), "{text}");
    }

    /// The bearer credential redacts identically: the Debug prints the
    /// VARIANT, never the token bytes.
    #[test]
    fn debug_redacts_the_token() {
        let auth = Auth::Token {
            token: "jwt-secret-bytes".to_string(),
        };
        assert_eq!(format!("{auth:?}"), "Auth::Token { .. }");
        let target = Target {
            endpoint: "http://h".into(),
            ns: "test".into(),
            db: "scratch".into(),
            auth,
        };
        let text = format!("{target:?}");
        assert!(text.contains("Auth::Token { .. }"), "{text}");
        assert!(!text.contains("jwt-secret-bytes"), "{text}");
    }

    /// The header spellings, one home ([`authorization_header`]):
    /// Basic is the docs' `-u` shorthand (`base64(user:pass)`), the
    /// token rides verbatim after `Bearer ` (probed 3.3.0: accepted,
    /// HTTP 200), and no credential sends NO header at all (the
    /// anonymous form).
    #[test]
    fn the_authorization_header_spells_each_credential() {
        assert_eq!(authorization_header(&Auth::None), None);
        assert_eq!(
            authorization_header(&Auth::Basic {
                user: "u".to_string(),
                pass: "p".to_string(),
            }),
            Some(format!("Basic {}", super::super::base64::encode(b"u:p"))),
        );
        assert_eq!(
            authorization_header(&Auth::Token {
                token: "tok".to_string(),
            }),
            Some("Bearer tok".to_string()),
        );
    }

    /// The token rides the same redaction guard the Basic pair does: a
    /// control character in the token refuses up front, naming the
    /// HEADER (never the value); a clean token validates.
    #[test]
    fn validate_guards_the_token_like_any_header_value() {
        let target_of = |token: &str| Target {
            endpoint: "http://h".into(),
            ns: "test".into(),
            db: "scratch".into(),
            auth: Auth::Token {
                token: token.to_string(),
            },
        };
        target_of("plain-jwt")
            .validate()
            .expect("a clean token validates");
        let error = target_of("bad\u{7}jwt")
            .validate()
            .expect_err("control char refuses");
        assert_eq!(
            error,
            TransportError::HeaderValueInvalid {
                header: "Authorization token".to_string(),
            },
            "the refusal names the header, never the value"
        );
    }
}
