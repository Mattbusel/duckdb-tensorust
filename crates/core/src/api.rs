//! Client for the Tensorust Data API at shop.tensorust.com.
//!
//! Nothing in here knows about DuckDB. Each public function makes one or a few
//! HTTPS calls and returns a [`Table`]: typed columns plus rows of [`Cell`]s.
//! The table functions in `functions.rs` only declare and emit what comes back.
//!
//! Two transports are used:
//! - the plain REST catalog (`/api/datasets`) for the dataset list and the free
//!   25-row samples, which never need a key;
//! - the MCP endpoint (`/api/mcp`, JSON-RPC `tools/call`) for credit marks,
//!   disagreements, Q3 cuts and exposure. Without a key it answers with the free
//!   preview. With an enterprise `tdk_` key in `Authorization: Bearer` it
//!   answers with the full record. `full=true` is never sent: the server unlocks
//!   full rows from the key alone, so a missing key can never turn into a paid
//!   per-call quote.

use serde_json::{Map, Value, json};
use std::env;
use std::time::Duration;

pub const DEFAULT_BASE_URL: &str = "https://shop.tensorust.com";
const TIMEOUT_SECS: u64 = 60;
const USER_AGENT: &str = concat!("duckdb-tensorust/", env!("CARGO_PKG_VERSION"));

/// Errors are plain strings; the DuckDB layer turns them into SQL errors.
pub type ApiResult<T> = Result<T, String>;

/// The DuckDB type of one output column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Varchar,
    Double,
    Bigint,
    Boolean,
    Date,
}

/// One value. `Null` is SQL NULL: a missing figure is never turned into 0.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Str(String),
    F64(f64),
    I64(i64),
    Bool(bool),
    /// Days since 1970-01-01.
    Date(i32),
}

/// Columns and rows ready to emit.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub columns: Vec<(String, Kind)>,
    pub rows: Vec<Vec<Cell>>,
}

/// Where the API lives. `TENSORUST_BASE_URL` overrides it for testing.
pub fn base_url() -> String {
    env::var("TENSORUST_BASE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// The API key from the environment, if any. The `tensorust_api_key` setting
/// takes precedence and is read by the DuckDB layer.
pub fn env_key() -> Option<String> {
    env::var("TENSORUST_API_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn checked_url(path: &str) -> ApiResult<String> {
    let url = format!("{}{}", base_url(), path);
    // The base URL is settable through the environment, so pin the scheme.
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!(
            "Tensorust base URL must be http or https, got {url}"
        ));
    }
    Ok(url)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .user_agent(USER_AGENT)
        .build()
}

fn transport_error(what: &str, err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => format!(
            "Tensorust rejected the API key for {what}. Check tensorust_api_key or \
             TENSORUST_API_KEY, or unset it to get the free preview."
        ),
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            let note = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .or_else(|| v.get("note"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            if note.is_empty() {
                format!("Tensorust request for {what} failed with HTTP {code}.")
            } else {
                format!("Tensorust request for {what} failed with HTTP {code}: {note}")
            }
        }
        other => format!("Tensorust request for {what} failed: {other}"),
    }
}

/// GET a REST path and return the body as text.
fn get_text(path: &str, what: &str) -> ApiResult<String> {
    let url = checked_url(path)?;
    agent()
        .get(&url)
        .call()
        .map_err(|e| transport_error(what, e))?
        .into_string()
        .map_err(|e| format!("Tensorust response for {what} could not be read: {e}"))
}

/// Call one MCP tool and return the JSON object its text content holds.
pub fn call_tool(tool: &str, arguments: Value, key: Option<&str>) -> ApiResult<Value> {
    let url = checked_url("/api/mcp")?;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    });
    let mut request = agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream");
    if let Some(key) = key {
        // A header, not a query parameter, so the key stays out of URLs and logs.
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    let response = request
        .send_string(&body.to_string())
        .map_err(|e| transport_error(tool, e))?;
    let text = response
        .into_string()
        .map_err(|e| format!("Tensorust response for {tool} could not be read: {e}"))?;
    parse_tool_reply(tool, &text)
}

/// Unwrap a JSON-RPC `tools/call` reply. Accepts a plain JSON body or a
/// single server-sent `data:` event.
pub fn parse_tool_reply(tool: &str, text: &str) -> ApiResult<Value> {
    let trimmed = text.trim();
    let json_text = if trimmed.starts_with('{') {
        trimmed.to_string()
    } else {
        trimmed
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("")
    };
    let reply: Value = serde_json::from_str(&json_text)
        .map_err(|e| format!("Tensorust returned an unreadable reply for {tool}: {e}"))?;
    if let Some(err) = reply.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("Tensorust {tool}: {msg}"));
    }
    let result = reply
        .get("result")
        .ok_or_else(|| format!("Tensorust {tool}: reply has no result"))?;
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|c| c.iter().find_map(|i| i.get("text").and_then(Value::as_str)))
        .unwrap_or("");
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(format!("Tensorust {tool}: {content}"));
    }
    serde_json::from_str(content).map_err(|_| format!("Tensorust {tool}: {content}"))
}

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

fn pick<'a>(obj: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .filter_map(|k| obj.get(*k))
        .find(|v| !v.is_null())
}

/// "full" when the server says so, otherwise "preview".
pub fn access_of(payload: &Value) -> String {
    match payload.get("access").and_then(Value::as_str) {
        Some("full") => "full".to_string(),
        _ => "preview".to_string(),
    }
}

/// Days since 1970-01-01 for a `YYYY-MM-DD` prefix, or `None`.
pub fn parse_date(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if b.len() > 10 && !matches!(b[10], b'T' | b' ') {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let m: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    i32::try_from(era * 146_097 + doe - 719_468).ok()
}

/// Convert one JSON value to a cell of the declared kind. Anything that does
/// not fit becomes NULL rather than a made-up value.
pub fn cell(value: Option<&Value>, kind: Kind) -> Cell {
    let Some(v) = value.filter(|v| !v.is_null()) else {
        return Cell::Null;
    };
    match kind {
        Kind::Double => v
            .as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .map_or(Cell::Null, Cell::F64),
        Kind::Bigint => v
            .as_i64()
            .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
            .or_else(|| v.as_array().map(|a| a.len() as i64))
            .map_or(Cell::Null, Cell::I64),
        Kind::Boolean => v.as_bool().map_or(Cell::Null, Cell::Bool),
        Kind::Date => v
            .as_str()
            .and_then(parse_date)
            .map_or(Cell::Null, Cell::Date),
        Kind::Varchar => match v {
            Value::String(s) => Cell::Str(s.clone()),
            Value::Array(items) => Cell::Str(
                items
                    .iter()
                    .map(|i| match i {
                        Value::String(s) => s.clone(),
                        other => other
                            .get("fund")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| other.to_string()),
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            other => Cell::Str(other.to_string()),
        },
    }
}

/// Build a table from flat JSON objects, reading each declared column by name.
pub fn table_from_objects(columns: &[(&str, Kind)], objects: &[Map<String, Value>]) -> Table {
    Table {
        columns: columns.iter().map(|(n, k)| (n.to_string(), *k)).collect(),
        rows: objects
            .iter()
            .map(|o| columns.iter().map(|(n, k)| cell(o.get(*n), *k)).collect())
            .collect(),
    }
}

fn object(pairs: Vec<(&str, Option<Value>)>) -> Map<String, Value> {
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.unwrap_or(Value::Null)))
        .collect()
}

fn get(v: &Value, keys: &[&str]) -> Option<Value> {
    pick(v, keys).cloned()
}

/// A share-of-par gap (0.802) as points (80.2).
fn gap_points(v: &Value) -> Option<Value> {
    if let Some(p) = pick(v, &["gap_points"]) {
        return Some(p.clone());
    }
    pick(v, &["gap"])
        .and_then(Value::as_f64)
        .map(|g| json!((g * 1000.0).round() / 10.0))
}

fn not_found(tool: &str, query: &str, payload: &Value) -> String {
    let similar: Vec<String> = payload
        .get("similar")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let note = payload
        .get("note")
        .and_then(Value::as_str)
        .unwrap_or("No match.");
    if similar.is_empty() {
        format!("Tensorust {tool}: '{query}' not found. {note}")
    } else {
        format!(
            "Tensorust {tool}: '{query}' not found. {note} Similar: {}",
            similar.join("; ")
        )
    }
}

// ---------------------------------------------------------------------------
// tensorust_lender_marks(name)
// ---------------------------------------------------------------------------

pub const LENDER_MARK_COLUMNS: &[(&str, Kind)] = &[
    ("company", Kind::Varchar),
    ("fund", Kind::Varchar),
    ("manager", Kind::Varchar),
    ("mark", Kind::Double),
    ("par", Kind::Double),
    ("prev_mark", Kind::Double),
    ("q3_mark", Kind::Double),
    ("q3_filed", Kind::Date),
    ("filing", Kind::Varchar),
    ("lowest_mark", Kind::Double),
    ("lowest_manager", Kind::Varchar),
    ("highest_mark", Kind::Double),
    ("highest_manager", Kind::Varchar),
    ("gap_points", Kind::Double),
    ("loan_par_usd", Kind::Double),
    ("maturity", Kind::Date),
    ("as_of", Kind::Date),
    ("access", Kind::Varchar),
];

/// One row per BDC fund holding the company's first-lien loan.
/// Preview: up to three funds. Full: every fund with par, prior mark, Q3 mark and filing.
pub fn lender_mark_rows(name: &str, payload: &Value) -> ApiResult<Vec<Map<String, Value>>> {
    if payload.get("found").and_then(Value::as_bool) != Some(true) {
        return Err(not_found("lender_marks", name, payload));
    }
    let access = access_of(payload);
    let lenders = pick(payload, &["lenders", "sample"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(lenders
        .iter()
        .map(|l| {
            object(vec![
                ("company", get(payload, &["company", "name"])),
                ("fund", get(l, &["fund"])),
                ("manager", get(l, &["manager"])),
                ("mark", get(l, &["mark"])),
                ("par", get(l, &["par"])),
                ("prev_mark", get(l, &["prev_mark"])),
                ("q3_mark", get(l, &["q3_mark"])),
                ("q3_filed", get(l, &["q3_filed"])),
                ("filing", get(l, &["filing"])),
                ("lowest_mark", get(payload, &["lowest_mark", "low"])),
                (
                    "lowest_manager",
                    get(payload, &["lowest_manager", "low_manager"]),
                ),
                ("highest_mark", get(payload, &["highest_mark", "high"])),
                (
                    "highest_manager",
                    get(payload, &["highest_manager", "high_manager"]),
                ),
                ("gap_points", gap_points(payload)),
                ("loan_par_usd", get(payload, &["loan_par_usd", "par"])),
                ("maturity", get(payload, &["maturity"])),
                ("as_of", get(payload, &["as_of"])),
                ("access", Some(json!(access))),
            ])
        })
        .collect())
}

pub fn lender_marks(name: &str, key: Option<&str>) -> ApiResult<Table> {
    let payload = call_tool("lender_marks", json!({ "name": name }), key)?;
    let rows = lender_mark_rows(name, &payload)?;
    Ok(table_from_objects(LENDER_MARK_COLUMNS, &rows))
}

// ---------------------------------------------------------------------------
// tensorust_credit_disagreements()
// ---------------------------------------------------------------------------

pub const DISAGREEMENT_COLUMNS: &[(&str, Kind)] = &[
    ("company", Kind::Varchar),
    ("gap_points", Kind::Double),
    ("lowest_mark", Kind::Double),
    ("lowest_manager", Kind::Varchar),
    ("highest_mark", Kind::Double),
    ("highest_manager", Kind::Varchar),
    ("loan_par_usd", Kind::Double),
    ("managers", Kind::Bigint),
    ("funds", Kind::Bigint),
    ("maturity", Kind::Date),
    ("as_of", Kind::Date),
    ("access", Kind::Varchar),
];

pub fn disagreement_rows(payload: &Value) -> Vec<Map<String, Value>> {
    let access = access_of(payload);
    payload
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|x| {
                    object(vec![
                        ("company", get(x, &["company", "name"])),
                        ("gap_points", gap_points(x)),
                        ("lowest_mark", get(x, &["lowest_mark", "low"])),
                        ("lowest_manager", get(x, &["lowest_manager", "low_manager"])),
                        ("highest_mark", get(x, &["highest_mark", "high"])),
                        (
                            "highest_manager",
                            get(x, &["highest_manager", "high_manager"]),
                        ),
                        ("loan_par_usd", get(x, &["loan_par_usd", "par"])),
                        ("managers", get(x, &["managers"])),
                        ("funds", get(x, &["funds"])),
                        ("maturity", get(x, &["maturity"])),
                        ("as_of", get(payload, &["as_of"])),
                        ("access", Some(json!(access))),
                    ])
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn credit_disagreements(key: Option<&str>) -> ApiResult<Table> {
    let payload = call_tool("credit_disagreements", json!({}), key)?;
    Ok(table_from_objects(
        DISAGREEMENT_COLUMNS,
        &disagreement_rows(&payload),
    ))
}

// ---------------------------------------------------------------------------
// tensorust_q3_cuts()
// ---------------------------------------------------------------------------

pub const Q3_CUT_COLUMNS: &[(&str, Kind)] = &[
    ("company", Kind::Varchar),
    ("fund", Kind::Varchar),
    ("manager", Kind::Varchar),
    ("jun30_mark", Kind::Double),
    ("sep30_mark", Kind::Double),
    ("change_points", Kind::Double),
    ("par", Kind::Double),
    ("filed", Kind::Date),
    ("holders_not_filed_yet_above", Kind::Bigint),
    ("pending_holders", Kind::Varchar),
    ("as_of", Kind::Date),
    ("access", Kind::Varchar),
];

pub fn q3_cut_rows(payload: &Value) -> Vec<Map<String, Value>> {
    let access = access_of(payload);
    payload
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|x| {
                    let pending = pick(x, &["not_filed_yet_above"]).cloned();
                    object(vec![
                        ("company", get(x, &["company"])),
                        ("fund", get(x, &["fund"])),
                        ("manager", get(x, &["manager"])),
                        ("jun30_mark", get(x, &["jun30"])),
                        ("sep30_mark", get(x, &["sep30"])),
                        ("change_points", get(x, &["change_points"])),
                        ("par", get(x, &["par"])),
                        ("filed", get(x, &["filed"])),
                        (
                            "holders_not_filed_yet_above",
                            pending
                                .clone()
                                .or_else(|| get(x, &["holders_not_filed_yet_above"])),
                        ),
                        ("pending_holders", pending),
                        ("as_of", get(payload, &["as_of"])),
                        ("access", Some(json!(access))),
                    ])
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn q3_cuts(key: Option<&str>) -> ApiResult<Table> {
    let payload = call_tool("credit_q3_cuts", json!({}), key)?;
    Ok(table_from_objects(Q3_CUT_COLUMNS, &q3_cut_rows(&payload)))
}

// ---------------------------------------------------------------------------
// tensorust_exposure(institution)
// ---------------------------------------------------------------------------

pub const EXPOSURE_COLUMNS: &[(&str, Kind)] = &[
    ("institution", Kind::Varchar),
    ("uei", Kind::Varchar),
    ("state", Kind::Varchar),
    ("award", Kind::Varchar),
    ("agency", Kind::Varchar),
    ("title", Kind::Varchar),
    ("fy25_obligations", Kind::Double),
    ("fy26_amount", Kind::Double),
    ("project_end", Kind::Date),
    ("vendors", Kind::Varchar),
    ("lists", Kind::Varchar),
    ("attribution", Kind::Varchar),
    ("papers", Kind::Bigint),
    ("latest_paper", Kind::Date),
    ("award_class", Kind::Varchar),
    ("evidence_level", Kind::Varchar),
    ("in_force_or_no_grace", Kind::Boolean),
    ("evidence", Kind::Varchar),
    ("evidence_paper", Kind::Varchar),
    ("awards_total", Kind::Bigint),
    ("fy25_total", Kind::Double),
    ("as_of", Kind::Date),
    ("access", Kind::Varchar),
];

/// One row per federal award whose 2025-26 papers name a listed vendor.
/// Preview: the institution's 5 largest. Full: every award with its evidence.
pub fn exposure_rows(name: &str, payload: &Value) -> ApiResult<Vec<Map<String, Value>>> {
    if payload.get("found").and_then(Value::as_bool) != Some(true) {
        return Err(not_found("exposure_institution", name, payload));
    }
    let access = access_of(payload);
    let items = payload
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .map(|a| {
            object(vec![
                ("institution", get(payload, &["institution"])),
                ("uei", get(payload, &["uei"])),
                ("state", get(payload, &["state"])),
                ("award", get(a, &["award"])),
                ("agency", get(a, &["agency"])),
                ("title", get(a, &["title"])),
                ("fy25_obligations", get(a, &["fy25_obligations"])),
                ("fy26_amount", get(a, &["fy26_amount"])),
                ("project_end", get(a, &["project_end"])),
                ("vendors", get(a, &["vendors"])),
                ("lists", get(a, &["lists"])),
                ("attribution", get(a, &["attribution"])),
                ("papers", get(a, &["papers"])),
                ("latest_paper", get(a, &["latest_paper"])),
                ("award_class", get(a, &["award_class"])),
                ("evidence_level", get(a, &["evidence_level"])),
                ("in_force_or_no_grace", get(a, &["in_force_or_no_grace"])),
                ("evidence", get(a, &["evidence"])),
                ("evidence_paper", get(a, &["evidence_paper"])),
                ("awards_total", get(payload, &["awards"])),
                ("fy25_total", get(payload, &["fy25"])),
                ("as_of", get(payload, &["as_of"])),
                ("access", Some(json!(access))),
            ])
        })
        .collect())
}

pub fn exposure(name: &str, key: Option<&str>) -> ApiResult<Table> {
    let payload = call_tool("exposure_institution", json!({ "name": name }), key)?;
    let rows = exposure_rows(name, &payload)?;
    Ok(table_from_objects(EXPOSURE_COLUMNS, &rows))
}

// ---------------------------------------------------------------------------
// tensorust_datasets(search := '', category := '')
// ---------------------------------------------------------------------------

pub const DATASET_COLUMNS: &[(&str, Kind)] = &[
    ("slug", Kind::Varchar),
    ("name", Kind::Varchar),
    ("category", Kind::Varchar),
    ("line", Kind::Varchar),
    ("rows", Kind::Bigint),
    ("tiers", Kind::Varchar),
    ("page", Kind::Varchar),
    ("sample_csv", Kind::Varchar),
    ("buy_url", Kind::Varchar),
];

/// Percent-encode a query or path value so it cannot open a second parameter.
pub fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn dataset_rows(payload: &Value) -> Vec<Map<String, Value>> {
    payload
        .get("datasets")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|d| {
                    let tiers = d.get("tiers").and_then(Value::as_array).map(|t| {
                        json!(
                            t.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("; ")
                        )
                    });
                    object(vec![
                        ("slug", get(d, &["slug"])),
                        ("name", get(d, &["name"])),
                        ("category", get(d, &["category"])),
                        ("line", get(d, &["line"])),
                        ("rows", get(d, &["rows"])),
                        ("tiers", tiers),
                        ("page", get(d, &["page"])),
                        ("sample_csv", get(d, &["sample_csv"])),
                        ("buy_url", get(d, &["buy_url"])),
                    ])
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn datasets(search: &str, category: &str) -> ApiResult<Table> {
    let mut rows = Vec::new();
    let mut offset: u64 = 0;
    // The catalog is about 145 entries; 20 pages of 50 is a generous ceiling.
    for _ in 0..20 {
        let path = format!(
            "/api/datasets?limit=50&offset={offset}&q={}&category={}",
            urlencode(search),
            urlencode(category)
        );
        let text = get_text(&path, "the dataset catalog")?;
        let payload: Value = serde_json::from_str(&text)
            .map_err(|e| format!("Tensorust dataset catalog was unreadable: {e}"))?;
        let page = dataset_rows(&payload);
        let got = page.len() as u64;
        rows.extend(page);
        match payload.get("next_offset").and_then(Value::as_u64) {
            Some(next) if got > 0 && next > offset => offset = next,
            _ => break,
        }
    }
    Ok(table_from_objects(DATASET_COLUMNS, &rows))
}

// ---------------------------------------------------------------------------
// tensorust_sample(slug)
// ---------------------------------------------------------------------------

fn looks_integer(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        // Leading zeros (ZIP codes, CIKs, FIPS) are identifiers, not numbers.
        && !(digits.len() > 1 && digits.starts_with('0'))
        && s.parse::<i64>().is_ok()
}

fn looks_double(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    !digits.is_empty()
        && digits
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
        && digits
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_digit() || b == b'.')
        && !(digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0."))
        && s.parse::<f64>().is_ok_and(f64::is_finite)
}

fn looks_bool(s: &str) -> bool {
    s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false")
}

fn looks_date(s: &str) -> bool {
    s.len() == 10 && parse_date(s).is_some()
}

/// Pick the narrowest type every non-empty value fits.
pub fn infer_kind(values: &[&str]) -> Kind {
    let present: Vec<&str> = values.iter().copied().filter(|v| !v.is_empty()).collect();
    if present.is_empty() {
        return Kind::Varchar;
    }
    if present.iter().all(|v| looks_integer(v)) {
        Kind::Bigint
    } else if present.iter().all(|v| looks_double(v)) {
        Kind::Double
    } else if present.iter().all(|v| looks_date(v)) {
        Kind::Date
    } else if present.iter().all(|v| looks_bool(v)) {
        Kind::Boolean
    } else {
        Kind::Varchar
    }
}

fn typed_cell(raw: &str, kind: Kind) -> Cell {
    if raw.is_empty() {
        return Cell::Null;
    }
    match kind {
        Kind::Bigint => raw.parse().map_or(Cell::Null, Cell::I64),
        Kind::Double => raw.parse().map_or(Cell::Null, Cell::F64),
        Kind::Date => parse_date(raw).map_or(Cell::Null, Cell::Date),
        Kind::Boolean => Cell::Bool(raw.eq_ignore_ascii_case("true")),
        Kind::Varchar => Cell::Str(raw.to_string()),
    }
}

/// Parse a CSV sample into a typed table. Empty or duplicate header names are
/// made unique so DuckDB can bind them.
pub fn table_from_csv(csv_text: &str) -> ApiResult<Table> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(csv_text.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| format!("Tensorust sample CSV has no readable header: {e}"))?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    let mut records: Vec<Vec<String>> = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| format!("Tensorust sample CSV is malformed: {e}"))?;
        let mut row: Vec<String> = record.iter().map(str::to_string).collect();
        row.resize(headers.len(), String::new());
        records.push(row);
    }

    let mut seen = std::collections::HashMap::<String, usize>::new();
    let mut columns = Vec::with_capacity(headers.len());
    for (i, h) in headers.iter().enumerate() {
        let base = if h.is_empty() {
            format!("column{i}")
        } else {
            h.clone()
        };
        let n = seen.entry(base.to_lowercase()).or_insert(0);
        *n += 1;
        let name = if *n == 1 { base } else { format!("{base}_{n}") };
        let values: Vec<&str> = records.iter().map(|r| r[i].as_str()).collect();
        columns.push((name, infer_kind(&values)));
    }
    let rows = records
        .iter()
        .map(|r| {
            columns
                .iter()
                .enumerate()
                .map(|(i, (_, kind))| typed_cell(&r[i], *kind))
                .collect()
        })
        .collect();
    Ok(Table { columns, rows })
}

pub fn sample(slug: &str) -> ApiResult<Table> {
    let slug = slug.trim();
    if slug.is_empty() {
        return Err("tensorust_sample needs a dataset slug; see tensorust_datasets()".into());
    }
    let path = format!("/api/datasets/{}?sample=1", urlencode(slug));
    let text = get_text(&path, &format!("the sample of '{slug}'"))?;
    if text.trim_start().starts_with('{') {
        // A JSON body here is the API explaining why there is no CSV.
        let note = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
            .unwrap_or(text);
        return Err(format!("Tensorust has no sample for '{slug}': {note}"));
    }
    table_from_csv(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_parse_to_epoch_days() {
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(parse_date("2026-06-30"), Some(20634));
        assert_eq!(parse_date("2026-11-13T10:00:00Z"), Some(20770));
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("20260630"), None);
        assert_eq!(parse_date("2026-06-301"), None);
    }

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(urlencode("bdc-nonaccrual-watch"), "bdc-nonaccrual-watch");
        assert_eq!(urlencode("a b&c=d/"), "a%20b%26c%3Dd%2F");
    }

    #[test]
    fn null_never_becomes_zero() {
        assert_eq!(cell(Some(&Value::Null), Kind::Double), Cell::Null);
        assert_eq!(cell(None, Kind::Bigint), Cell::Null);
        assert_eq!(cell(Some(&json!("n/a")), Kind::Double), Cell::Null);
    }

    #[test]
    fn tool_reply_is_unwrapped_and_errors_surface() {
        let ok = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{\"found\":true}"}],"isError":false}}"#;
        assert_eq!(parse_tool_reply("t", ok).unwrap(), json!({"found": true}));
        let sse = format!("event: message\ndata: {ok}\n\n");
        assert_eq!(parse_tool_reply("t", &sse).unwrap(), json!({"found": true}));
        let err = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"Unknown list X"}],"isError":true}}"#;
        assert!(
            parse_tool_reply("t", err)
                .unwrap_err()
                .contains("Unknown list X")
        );
        let rpc = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Unknown tool"}}"#;
        assert!(
            parse_tool_reply("t", rpc)
                .unwrap_err()
                .contains("Unknown tool")
        );
    }

    #[test]
    fn lender_marks_preview_and_full_share_columns() {
        let preview = json!({
            "found": true, "access": "preview", "company": "Assembly Intermediate LLC",
            "as_of": "2026-06-30", "loan_par_usd": 53825000, "maturity": "2028-10-19",
            "lowest_mark": 0.198, "lowest_manager": "Goldman", "highest_mark": 1,
            "highest_manager": "Morgan Stanley", "gap_points": 80.2,
            "sample": [{"fund": "GOLDMAN SACHS BDC, INC.", "manager": "Goldman", "mark": 0.198}]
        });
        let t = table_from_objects(
            LENDER_MARK_COLUMNS,
            &lender_mark_rows("x", &preview).unwrap(),
        );
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0][0], Cell::Str("Assembly Intermediate LLC".into()));
        assert_eq!(t.rows[0][3], Cell::F64(0.198));
        assert_eq!(t.rows[0][4], Cell::Null); // par is full-only
        assert_eq!(t.rows[0][17], Cell::Str("preview".into()));

        let full = json!({
            "found": true, "access": "full", "name": "Assembly Intermediate LLC",
            "low": 0.198, "high": 1, "gap": 0.802, "low_manager": "Goldman",
            "high_manager": "Morgan Stanley", "par": 53825000, "maturity": "2028-10-19",
            "as_of": "2026-06-30",
            "lenders": [{"fund": "F", "manager": "Goldman", "mark": 0.198, "par": 100,
                         "prev_mark": 0.9, "filing": "https://sec.gov/x", "q3_mark": null,
                         "q3_filed": "2026-11-12"}]
        });
        let t = table_from_objects(LENDER_MARK_COLUMNS, &lender_mark_rows("x", &full).unwrap());
        assert_eq!(t.rows[0][0], Cell::Str("Assembly Intermediate LLC".into()));
        assert_eq!(t.rows[0][4], Cell::F64(100.0));
        assert_eq!(t.rows[0][6], Cell::Null);
        assert_eq!(t.rows[0][7], Cell::Date(20769));
        assert_eq!(t.rows[0][13], Cell::F64(80.2));
        assert_eq!(t.rows[0][17], Cell::Str("full".into()));
    }

    #[test]
    fn unknown_company_is_an_error_with_suggestions() {
        let miss = json!({"found": false, "note": "No BDC first-lien term loan to this name.",
                          "similar": [{"name": "Pluralsight LLC"}]});
        let err = lender_mark_rows("Plural", &miss).unwrap_err();
        assert!(err.contains("Pluralsight LLC"));
    }

    #[test]
    fn q3_pending_holders_count_in_preview_and_full() {
        let preview = json!({"as_of": "2026-09-30", "items": [
            {"company": "C", "fund": "F", "jun30": 1, "sep30": 0.4, "change_points": -60,
             "par": 5, "holders_not_filed_yet_above": 2}]});
        let t = table_from_objects(Q3_CUT_COLUMNS, &q3_cut_rows(&preview));
        assert_eq!(t.rows[0][8], Cell::I64(2));
        assert_eq!(t.rows[0][9], Cell::Null);
        let full = json!({"as_of": "2026-09-30", "access": "full", "items": [
            {"company": "C", "fund": "F", "manager": "M", "jun30": 1, "sep30": 0.4,
             "change_points": -60, "par": 5, "filed": "2026-11-05",
             "not_filed_yet_above": [{"fund": "A", "jun30": 1, "par": 1}, {"fund": "B", "jun30": 1, "par": 1}]}]});
        let t = table_from_objects(Q3_CUT_COLUMNS, &q3_cut_rows(&full));
        assert_eq!(t.rows[0][8], Cell::I64(2));
        assert_eq!(t.rows[0][9], Cell::Str("A, B".into()));
        assert_eq!(t.rows[0][11], Cell::Str("full".into()));
    }

    #[test]
    fn disagreement_full_gap_is_converted_to_points() {
        let full = json!({"as_of": "2026-06-30", "access": "full", "items": [
            {"name": "C", "gap": 0.4031, "low": 0.5, "high": 0.9, "low_manager": "A",
             "high_manager": "B", "par": 10, "managers": 2, "funds": 3, "maturity": null}]});
        let t = table_from_objects(DISAGREEMENT_COLUMNS, &disagreement_rows(&full));
        assert_eq!(t.rows[0][1], Cell::F64(40.3));
        assert_eq!(t.rows[0][7], Cell::I64(2));
    }

    #[test]
    fn csv_samples_get_types_and_unique_names() {
        let csv = "cik,name,par,mark,as_of,flag,name\n0001,A Co,100,0.95,2026-06-30,true,x\n0002,\"B, Inc\",,0.5,2026-06-30,false,y\n";
        let t = table_from_csv(csv).unwrap();
        let kinds: Vec<Kind> = t.columns.iter().map(|c| c.1).collect();
        assert_eq!(
            kinds,
            vec![
                Kind::Varchar,
                Kind::Varchar,
                Kind::Bigint,
                Kind::Double,
                Kind::Date,
                Kind::Boolean,
                Kind::Varchar
            ]
        );
        assert_eq!(t.columns[6].0, "name_2");
        assert_eq!(t.rows[1][1], Cell::Str("B, Inc".into()));
        assert_eq!(t.rows[1][2], Cell::Null);
        assert_eq!(t.rows[0][0], Cell::Str("0001".into()));
    }
}
