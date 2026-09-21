//! Test fixture, and the thing the demo points at.
//!
//! Two jobs. For the test suite it is the same dumb echo/fail/flaky server it
//! has always been. For the demo it also *says what it did* — every handled
//! request is printed as a plain sentence and kept in a small in-memory log at
//! `GET /_log`, so the demo page can show the receiving end of the delivery
//! rather than asking the room to take Invokr's word for it.

use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many handled requests to keep. A demo never looks further back than the
/// scene it is in.
const LOG_CAPACITY: usize = 200;

/// How many long-running tasks to keep answers for. Oldest is dropped first,
/// so a process that runs all day does not grow one accepted task at a time.
const ASYNC_CAPACITY: usize = 64;

/// How many polled subjects (mandates, rides) to remember. Same reasoning.
const POLL_CAPACITY: usize = 64;

#[derive(Clone)]
struct AppState {
    request_count: Arc<AtomicU64>,
    seq: Arc<AtomicU64>,
    log: Arc<Mutex<VecDeque<Received>>>,
    /// Scripted answers for long-running tasks, oldest first.
    async_jobs: Arc<Mutex<AsyncJobs>>,
    async_seq: Arc<AtomicU64>,
    /// Progress of each thing being polled for a status, oldest first.
    polls: Arc<Mutex<Polls>>,
}

/// How far along one polled subject is: how many times its status has been
/// asked for, and how many failures it still owes before it answers at all.
#[derive(Default, Clone, Copy)]
struct Poll {
    checks: u64,
    failures: u64,
}

/// Subjects being polled, keyed by their id, oldest first.
type Polls = VecDeque<(String, Poll)>;

/// Accepted long-running tasks and the answers each still owes, oldest first.
type AsyncJobs = VecDeque<(String, Vec<ScriptStep>)>;

/// One scripted answer from a long-running task: a status, a body, and
/// optionally how long the caller should wait before asking again.
#[derive(Clone, Deserialize)]
struct ScriptStep {
    status: u16,
    #[serde(default)]
    body: serde_json::Value,
    #[serde(default)]
    retry_after: Option<u64>,
}

/// One handled request, as the receiving service would describe it.
#[derive(Clone, Serialize)]
struct Received {
    seq: u64,
    at: String,
    method: String,
    path: String,
    status: u16,
    ok: bool,
    /// What the service did about it, in a sentence.
    summary: String,
    /// The de-duplication key Invokr sent, if any.
    idempotency_key: Option<String>,
    /// Whether the call carried credentials. The value is never recorded — the
    /// whole point of the secret store is that it does not end up in logs.
    authenticated: bool,
    body: serde_json::Value,
}

#[derive(Serialize)]
struct EchoResponse {
    method: String,
    path: String,
    headers: std::collections::HashMap<String, String>,
    body: serde_json::Value,
}

#[derive(Deserialize)]
struct DelayQuery {
    ms: Option<u64>,
}

#[derive(Deserialize)]
struct FailQuery {
    code: Option<u16>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct SucceedAfterQuery {
    succeed_after: Option<u64>,
}

/// How a polled subject should behave: how many checks before it reaches a
/// terminal status, and how many times it should fail outright first.
#[derive(Deserialize)]
struct PollQuery {
    terminal_after: Option<u64>,
    fail_times: Option<u64>,
}

fn header(req: &HttpRequest, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn str_field<'a>(body: &'a serde_json::Value, key: &str, fallback: &'a str) -> &'a str {
    body.get(key).and_then(|v| v.as_str()).unwrap_or(fallback)
}

/// Record and print what just happened. The printed line and the logged
/// `summary` are the same sentence on purpose: what the room sees on the demo
/// page is what the service actually said in its own terminal.
fn record(
    state: &AppState,
    req: &HttpRequest,
    status: u16,
    summary: impl Into<String>,
    body: &serde_json::Value,
) {
    let summary = summary.into();
    let now = chrono::Local::now();
    let entry = Received {
        seq: state.seq.fetch_add(1, Ordering::SeqCst) + 1,
        at: now.to_rfc3339(),
        method: req.method().to_string(),
        path: req.path().to_string(),
        status,
        ok: (200..300).contains(&status),
        summary: summary.clone(),
        idempotency_key: header(req, "x-invokr-idempotency-key"),
        authenticated: header(req, "authorization").is_some(),
        body: body.clone(),
    };

    println!(
        "  {}  {} {}  {}{}",
        now.format("%H:%M:%S%.3f"),
        if entry.ok { "✓" } else { "✗" },
        status,
        summary,
        entry
            .idempotency_key
            .as_ref()
            .map(|k| format!("   [key {k}]"))
            .unwrap_or_default(),
    );

    if let Ok(mut log) = state.log.lock() {
        if log.len() >= LOG_CAPACITY {
            log.pop_front();
        }
        log.push_back(entry);
    }
}

// ─── the two routes the demo points at ───────────────────────────────────────

/// Stands in for an email service. Prints who it emailed.
async fn send_welcome_email(
    state: web::Data<AppState>,
    req: HttpRequest,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let body = body.into_inner();
    let customer = str_field(&body, "customer", "a customer");
    let email = str_field(&body, "email", "unknown@example.com");
    let order = str_field(&body, "order_id", "an order");

    let message_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    // `sent_by` comes from the calling team's own config, so printing it is what
    // makes two tenants firing the same endpoint name visibly different here.
    let from = body
        .get("sent_by")
        .and_then(|v| v.as_str())
        .map(|s| format!(", from {s}"))
        .unwrap_or_default();

    record(
        &state,
        &req,
        200,
        format!("Sent the welcome email to {customer} <{email}> about {order}{from}"),
        &body,
    );

    HttpResponse::Ok().json(serde_json::json!({
        "message_id": message_id,
        "delivered_to": email,
        "subject": format!("Welcome, {customer}"),
    }))
}

/// Stands in for a payment processor, and fails the first couple of times so
/// the retry scene has something real to retry. `?succeed_after=3` fails twice.
async fn charge(
    state: web::Data<AppState>,
    req: HttpRequest,
    query: web::Query<SucceedAfterQuery>,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let body = body.into_inner();
    let succeed_after = query.succeed_after.unwrap_or(3);
    let attempt = state.request_count.fetch_add(1, Ordering::SeqCst) + 1;

    let order = str_field(&body, "order_id", "an order");
    let amount = body
        .get("amount")
        .map(|v| v.to_string().trim_matches('"').to_string())
        .unwrap_or_else(|| "0".into());

    if attempt >= succeed_after {
        record(
            &state,
            &req,
            200,
            format!("Charged {amount} to {order} (try {attempt})"),
            &body,
        );
        HttpResponse::Ok().json(serde_json::json!({
            "captured": true,
            "amount": amount,
            "order_id": order,
            "try": attempt,
        }))
    } else {
        record(
            &state,
            &req,
            500,
            format!("Refused {order} — card processor timed out (try {attempt})"),
            &body,
        );
        HttpResponse::InternalServerError().json(serde_json::json!({
            "captured": false,
            "error": "upstream card processor timed out",
            "try": attempt,
        }))
    }
}

/// Stands in for whatever a recurring job pokes. Prints the tick.
async fn heartbeat(
    state: web::Data<AppState>,
    req: HttpRequest,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let body = body.into_inner();
    record(
        &state,
        &req,
        200,
        "Ran the every-minute health sweep".to_string(),
        &body,
    );
    HttpResponse::Ok().json(serde_json::json!({ "swept": true }))
}

// ─── the log ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LogQuery {
    /// Only entries newer than this sequence number.
    since: Option<u64>,
    limit: Option<usize>,
}

async fn read_log(state: web::Data<AppState>, query: web::Query<LogQuery>) -> HttpResponse {
    let since = query.since.unwrap_or(0);
    let limit = query.limit.unwrap_or(50);
    let entries: Vec<Received> = match state.log.lock() {
        Ok(log) => log
            .iter()
            .filter(|e| e.seq > since)
            .rev()
            .take(limit)
            .cloned()
            .collect(),
        Err(_) => Vec::new(),
    };
    HttpResponse::Ok().json(serde_json::json!({ "data": entries }))
}

async fn clear_log(state: web::Data<AppState>) -> HttpResponse {
    if let Ok(mut log) = state.log.lock() {
        log.clear();
    }
    HttpResponse::Ok().json(serde_json::json!({ "cleared": true }))
}

// ─── original fixture routes (the test suite depends on these) ───────────────

/// Always returns 200 OK with the received body echoed back.
async fn success(
    state: web::Data<AppState>,
    req: HttpRequest,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let body = body.into_inner();
    let headers: std::collections::HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    record(&state, &req, 200, "Accepted POST /success", &body);

    HttpResponse::Ok().json(EchoResponse {
        method: req.method().to_string(),
        path: req.path().to_string(),
        headers,
        body,
    })
}

/// Always returns an error. Use ?code=500&message=oops to customize.
async fn fail(
    state: web::Data<AppState>,
    req: HttpRequest,
    query: web::Query<FailQuery>,
) -> HttpResponse {
    let code = query.code.unwrap_or(500);
    let message = query
        .message
        .clone()
        .unwrap_or_else(|| "Simulated failure".into());

    record(
        &state,
        &req,
        code,
        format!("Returned {code} on purpose"),
        &serde_json::Value::Null,
    );

    HttpResponse::build(
        actix_web::http::StatusCode::from_u16(code)
            .unwrap_or(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR),
    )
    .json(serde_json::json!({
        "error": {
            "code": format!("MOCK_{}", code),
            "message": message,
        }
    }))
}

/// Responds after a configurable delay. Use ?ms=2000 for 2-second delay.
async fn slow(
    state: web::Data<AppState>,
    req: HttpRequest,
    query: web::Query<DelayQuery>,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let delay = query.ms.unwrap_or(3000);
    tokio::time::sleep(Duration::from_millis(delay)).await;
    let body = body.into_inner();
    record(
        &state,
        &req,
        200,
        format!("Waited {delay}ms, then answered"),
        &body,
    );
    HttpResponse::Ok().json(serde_json::json!({
        "delayed_ms": delay,
        "body": body,
    }))
}

/// Echoes the full request back (method, path, headers, body).
async fn echo(
    state: web::Data<AppState>,
    req: HttpRequest,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let body = body.into_inner();
    let headers: std::collections::HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    record(
        &state,
        &req,
        200,
        format!("Echoed {} {}", req.method(), req.path()),
        &body,
    );

    HttpResponse::Ok().json(EchoResponse {
        method: req.method().to_string(),
        path: req.path().to_string(),
        headers,
        body,
    })
}

/// Fails on the first N-1 requests and succeeds on the Nth.
/// Use ?succeed_after=3 to fail twice then succeed.
async fn flaky(
    state: web::Data<AppState>,
    req: HttpRequest,
    query: web::Query<std::collections::HashMap<String, String>>,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let succeed_after: u64 = query
        .get("succeed_after")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    let count = state.request_count.fetch_add(1, Ordering::SeqCst) + 1;
    let body = body.into_inner();

    if count >= succeed_after {
        record(
            &state,
            &req,
            200,
            format!("Succeeded on try {count}"),
            &body,
        );
        HttpResponse::Ok().json(serde_json::json!({
            "attempt": count,
            "status": "success",
            "body": body,
        }))
    } else {
        record(&state, &req, 500, format!("Failed on try {count}"), &body);
        HttpResponse::InternalServerError().json(serde_json::json!({
            "attempt": count,
            "status": "failed",
            "error": format!("Failing until attempt {}", succeed_after),
        }))
    }
}

/// Reset the flaky endpoint counter.
async fn reset_flaky(state: web::Data<AppState>) -> HttpResponse {
    state.request_count.store(0, Ordering::SeqCst);
    HttpResponse::Ok().json(serde_json::json!({ "reset": true }))
}

// ─── long-running tasks ──────────────────────────────────────────────────────
//
// Stands in for a destination that cannot answer straight away: it takes the
// work, replies 202 with a `Location` to poll, and hands out the scripted
// answers one at a time. The script comes from the request body, so a demo or a
// test decides how many times it says "still working" before it finishes.

/// Accept a long-running task and return where to check on it.
async fn async_start(
    state: web::Data<AppState>,
    req: HttpRequest,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let body = body.into_inner();
    let id = format!(
        "task-{}",
        state.async_seq.fetch_add(1, Ordering::SeqCst) + 1
    );

    let script: Vec<ScriptStep> = body
        .get("script")
        .and_then(|s| serde_json::from_value(s.clone()).ok())
        .unwrap_or_else(|| {
            vec![ScriptStep {
                status: 200,
                body: serde_json::json!({ "state": "done" }),
                retry_after: None,
            }]
        });

    let pending = script.iter().filter(|s| s.status == 202).count();
    if let Ok(mut jobs) = state.async_jobs.lock() {
        if jobs.len() >= ASYNC_CAPACITY {
            jobs.pop_front();
        }
        jobs.push_back((id.clone(), script));
    }

    record(
        &state,
        &req,
        202,
        format!(
            "Took on {} — working on it, ask me again ({pending} more to go)",
            str_field(&body, "job", "a long job")
        ),
        &body,
    );

    HttpResponse::Accepted()
        .insert_header(("Location", format!("/async/status/{id}")))
        .json(serde_json::json!({ "task_id": id, "state": "working" }))
}

/// Answer a check-in, consuming one scripted step. The last step repeats, so a
/// late poll still gets the terminal answer.
async fn async_status(
    state: web::Data<AppState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let id = path.into_inner();
    let step = {
        let Ok(mut jobs) = state.async_jobs.lock() else {
            return HttpResponse::InternalServerError().finish();
        };
        let Some((_, script)) = jobs.iter_mut().find(|(known, _)| *known == id) else {
            return HttpResponse::NotFound().json(serde_json::json!({ "error": "no such task" }));
        };
        if script.len() > 1 {
            script.remove(0)
        } else {
            script[0].clone()
        }
    };

    record(
        &state,
        &req,
        step.status,
        if step.status == 202 {
            format!("Checked on {id} — still working")
        } else {
            format!("Checked on {id} — finished")
        },
        &serde_json::Value::Null,
    );

    let mut res = HttpResponse::build(
        actix_web::http::StatusCode::from_u16(step.status)
            .unwrap_or(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR),
    );
    if let Some(after) = step.retry_after {
        res.insert_header(("Retry-After", after.to_string()));
    }
    res.json(step.body)
}

/// Give up on a long-running task.
async fn async_cancel(
    state: web::Data<AppState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let id = path.into_inner();
    if let Ok(mut jobs) = state.async_jobs.lock() {
        jobs.retain(|(known, _)| *known != id);
    }
    record(
        &state,
        &req,
        204,
        format!("Dropped {id} — the caller cancelled"),
        &serde_json::Value::Null,
    );
    HttpResponse::NoContent().finish()
}

// ─── polled statuses ─────────────────────────────────────────────────────────
//
// Stands in for a service that owns a long-lived thing and is asked, over and
// over, whether it is done yet: a mandate with a bank, a ride with a driver.
// The caller keeps asking until the answer is terminal, then stops asking.
//
// That is the shape of a scheduled job that cancels itself, so these routes are
// what the demo points at. `terminal_after` says how many checks it takes,
// `fail_times` makes the first N calls fail outright so retries have something
// real to retry. Either can come as a query parameter or as an `x-` header,
// because an endpoint's URL is often fixed while its headers are templated.

/// Advance one subject and say where it got to, and whether this call is one of
/// the failures it still owed.
fn advance(state: &AppState, id: &str, fail_times: u64) -> Option<(Poll, bool)> {
    let mut polls = state.polls.lock().ok()?;
    if !polls.iter().any(|(known, _)| known == id) {
        if polls.len() >= POLL_CAPACITY {
            polls.pop_front();
        }
        polls.push_back((id.to_string(), Poll::default()));
    }
    let (_, p) = polls.iter_mut().find(|(known, _)| known == id)?;
    let failing = p.failures < fail_times;
    if failing {
        p.failures += 1;
    } else {
        p.checks += 1;
    }
    Some((*p, failing))
}

/// Who asked. The endpoint templates this out of its own workspace's config, so
/// two teams firing the same endpoint name are visibly different here.
fn asked_by(req: &HttpRequest) -> String {
    header(req, "x-team")
        .map(|t| format!(", for the {t} team"))
        .unwrap_or_default()
}

/// A dial the caller can turn from the query string or from a header.
fn dial(req: &HttpRequest, from_query: Option<u64>, name: &str, fallback: u64) -> u64 {
    from_query
        .or_else(|| header(req, name).and_then(|v| v.trim().parse().ok()))
        .unwrap_or(fallback)
}

/// Sync a mandate's registration status with the bank.
async fn mandate_sync(
    state: web::Data<AppState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PollQuery>,
    body: Option<web::Json<serde_json::Value>>,
) -> HttpResponse {
    let id = path.into_inner();
    let body = body
        .map(|b| b.into_inner())
        .unwrap_or(serde_json::Value::Null);
    let terminal_after = dial(&req, query.terminal_after, "x-terminal-after", 3).max(1);
    let fail_times = dial(&req, query.fail_times, "x-fail-times", 0);
    let team = asked_by(&req);

    let Some((p, failing)) = advance(&state, &id, fail_times) else {
        return HttpResponse::InternalServerError().finish();
    };

    if failing {
        record(
            &state,
            &req,
            502,
            format!(
                "Bank gateway timed out on {id}{team} — nothing synced (try {})",
                p.failures
            ),
            &body,
        );
        return HttpResponse::BadGateway()
            .json(serde_json::json!({ "error": "bank gateway timed out", "mandate_id": id }));
    }

    let terminal = p.checks >= terminal_after;
    let status = if terminal { "ACTIVE" } else { "PENDING" };
    record(
        &state,
        &req,
        200,
        if terminal {
            format!("Asked the bank about {id}{team} — ACTIVE, the mandate is registered")
        } else {
            format!(
                "Asked the bank about {id}{team} — still PENDING (check {} of {terminal_after})",
                p.checks
            )
        },
        &body,
    );

    HttpResponse::Ok().json(serde_json::json!({
        "mandate_id": id,
        "status": status,
        "terminal": terminal,
        "checks": p.checks,
    }))
}

/// Ask Namma Yatri where a ride got to, and whether a tip was contributed.
async fn ride_status(
    state: web::Data<AppState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PollQuery>,
    body: Option<web::Json<serde_json::Value>>,
) -> HttpResponse {
    let id = path.into_inner();
    let body = body
        .map(|b| b.into_inner())
        .unwrap_or(serde_json::Value::Null);
    let terminal_after = dial(&req, query.terminal_after, "x-terminal-after", 3).max(1);
    let team = asked_by(&req);

    let Some((p, _)) = advance(&state, &id, 0) else {
        return HttpResponse::InternalServerError().finish();
    };

    let terminal = p.checks >= terminal_after;
    let status = if terminal { "COMPLETED" } else { "ON_TRIP" };
    record(
        &state,
        &req,
        200,
        if terminal {
            format!("Ride {id}{team} — COMPLETED, ₹30 tip went to the driver")
        } else {
            format!(
                "Ride {id}{team} — still ON_TRIP (check {} of {terminal_after})",
                p.checks
            )
        },
        &body,
    );

    HttpResponse::Ok().json(serde_json::json!({
        "ride_id": id,
        "status": status,
        "terminal": terminal,
        "tip_contributed": terminal,
        "tip_amount": if terminal { 30 } else { 0 },
        "checks": p.checks,
    }))
}

/// Forget every subject's progress, so a rehearsal starts from check one.
async fn reset_polls(state: web::Data<AppState>) -> HttpResponse {
    if let Ok(mut polls) = state.polls.lock() {
        polls.clear();
    }
    HttpResponse::Ok().json(serde_json::json!({ "reset": true }))
}

/// Health check.
async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .json()
        .init();

    let port: u16 = std::env::var("MOCK_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9999);

    let state = AppState {
        request_count: Arc::new(AtomicU64::new(0)),
        seq: Arc::new(AtomicU64::new(0)),
        log: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAPACITY))),
        async_jobs: Arc::new(Mutex::new(VecDeque::with_capacity(ASYNC_CAPACITY))),
        async_seq: Arc::new(AtomicU64::new(0)),
        polls: Arc::new(Mutex::new(VecDeque::with_capacity(POLL_CAPACITY))),
    };

    println!("\n  target service listening on :{port}");
    println!("  everything it receives gets printed here.\n");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .route("/health", web::get().to(health))
            // What the demo points at: a service that owns something slow and
            // is polled until the answer is terminal
            .route("/mandates/{id}/sync", web::post().to(mandate_sync))
            .route("/rides/{id}/status", web::post().to(ride_status))
            .route("/_polls/reset", web::post().to(reset_polls))
            .route("/emails/welcome", web::post().to(send_welcome_email))
            .route("/billing/charge", web::post().to(charge))
            .route("/ops/heartbeat", web::post().to(heartbeat))
            // Long-running destination: 202 + Location, then scripted check-ins
            .route("/async/start", web::post().to(async_start))
            .route("/async/status/{id}", web::get().to(async_status))
            .route("/async/status/{id}", web::delete().to(async_cancel))
            .route("/_log", web::get().to(read_log))
            .route("/_log/clear", web::post().to(clear_log))
            // Fixture routes used by the test suite
            .route("/success", web::post().to(success))
            .route("/fail", web::post().to(fail))
            .route("/fail", web::get().to(fail))
            .route("/slow", web::post().to(slow))
            .route("/echo", web::post().to(echo))
            .route("/echo", web::put().to(echo))
            .route("/flaky", web::post().to(flaky))
            .route("/flaky/reset", web::post().to(reset_flaky))
    })
    .bind(format!("0.0.0.0:{}", port))?
    .run()
    .await
}
