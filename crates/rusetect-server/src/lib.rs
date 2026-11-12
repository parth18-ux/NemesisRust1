//! RuseTect API (port of service/server.py): layered filtration cascade for mule accounts and scam money flows.
//!
//!   /                    landing page (links to the console, report and API docs)
//!   /console             Live Console: replays recorded IBM AMLworld (synthetic) test-period payments through the live path
//!   /report              benchmark report (21 AML models, 8 onboarding models)
//!   /api/health          engine status
//!   /api/v2/message      L-1 lure detector: score an SMS / WhatsApp / e-mail / call transcript, decide whether to warn
//!   /api/v2/payment      score one payment (L1 -> L2 -> decision band -> L4 case), escalated by recent lures
//!   /api/v2/assess       payment cascade + context layer + session domain, signed verdict, review alert
//!   /api/v2/onboard      L0 onboarding gate for a new account application (Feedzai BAF schema)
//!   /docs                OpenAPI docs (Swagger UI), /openapi.json
//!
//! All state lives behind one mutex, so requests are applied one at a time exactly as the single-threaded Python
//! event loop applied them; scoring takes microseconds, so this is not a bottleneck.
// Deliberate: comparisons are written NaN-aware (`!(a > b)`), branches mirror the Python they port line by line,
// and min/max chains keep Python's NaN behaviour where clamp() would panic or differ.
#![allow(clippy::neg_cmp_op_on_partial_ord, clippy::if_same_then_else, clippy::manual_clamp, clippy::type_complexity)]
#![allow(clippy::doc_lazy_continuation, clippy::doc_overindented_list_items)]
pub mod inputs;
pub mod openapi;
pub mod scale;
pub mod validate;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use indexmap::IndexMap;
use inputs::{AssessIn, PaymentIn, SessionIn};
use rusetect::behaviour::{TypingProfile, Verifier};
use rusetect::branding::{NAME, SLUG, TAGLINE, VERSION};
use rusetect::consortium::Consortium;
use rusetect::context_rules as ctx;
use rusetect::coordination::CoordinationTracker;
use rusetect::intervention::{plan, DuressVault};
use rusetect::lure_fusion::LureMemory;
use rusetect::payment::{read_demo_rows, DemoRow, Payment};
use rusetect::pyfmt::{dumps, jf, py_round, RESPONSE_STYLE};
use rusetect::review::{ReviewDesk, ReviewError};
use rusetect::scam_text::MessageModel;
use rusetect::security::{allowed, analyst_for, NonceCache, RateLimiter, Signer, KEYS};
use rusetect::session_signals::{assess_session, identity_check};
use rusetect::{paths, Engine};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use validate::{Errors, Obj};

pub const THIS_BANK: &str = "BANK-A";
/// first 100K rows of demo_stream.csv.gz are pre-test history
pub const DEMO_WARM: usize = 100_000;

// ---------------------------------------------------------------- state

#[derive(Default)]
pub struct Demo {
    pub rows: Option<Arc<Vec<DemoRow>>>,
    pub ptr: usize,
    pub warm: usize,
    pub truth: IndexMap<&'static str, i64>,
    pub feed: Vec<Value>,
}

pub struct AppState {
    pub root: PathBuf,
    pub engine: Option<Engine>,
    pub messages: Option<MessageModel>,
    pub verifier: Option<Verifier>,
    pub lures: LureMemory,
    pub desk: ReviewDesk,
    pub coord: CoordinationTracker,
    pub duress: DuressVault,
    pub signer: Signer,
    pub nonces: NonceCache,
    pub consortium: Consortium,
    pub profiles: HashMap<String, TypingProfile>,
    /// PRAMAAN account-takeover authentication (allow / challenge / block); None if its model files are absent
    pub auth: Option<rusetect::pramaan::AuthEngine>,
    pub demo: Demo,
}

pub struct Shared {
    pub state: Mutex<AppState>,
    pub rate: Mutex<RateLimiter>,
    /// repository root (pages, fixtures), fixed at start-up and read without taking the state lock
    pub root: PathBuf,
}

pub type AppRef = Arc<Shared>;

fn truth0() -> IndexMap<&'static str, i64> {
    [("tp", 0), ("fp", 0), ("fn", 0), ("pos", 0)].into_iter().collect()
}

impl AppState {
    pub fn load() -> Self {
        let root = paths::repo_root();
        let models = root.join("service").join("models");
        let engine = match Engine::new(&models, true) {
            Ok(e) => Some(e),
            Err(e) => {
                eprintln!("{NAME} engine unavailable: {e:#}");
                None
            }
        };
        let messages = match MessageModel::load(&models.join("message_model.json")) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("{NAME} message model unavailable: {e:#}");
                None
            }
        };
        AppState {
            verifier: Verifier::load(&models.join("typing_verifier.json")),
            root,
            engine,
            messages,
            lures: LureMemory::new(),
            desk: ReviewDesk::new(),
            coord: CoordinationTracker::default(),
            duress: DuressVault::new(),
            signer: Signer::from_env(),
            nonces: NonceCache::default(),
            consortium: Consortium::demo(),
            profiles: HashMap::new(),
            auth: match rusetect::pramaan::AuthModel::load(&models) {
                Ok(m) => m.map(rusetect::pramaan::AuthEngine::new),
                Err(e) => {
                    eprintln!("{NAME} authentication model unavailable: {e:#}");
                    None
                }
            },
            demo: Demo { truth: truth0(), ..Default::default() },
        }
    }

    fn need_engine(&mut self) -> Result<&mut Engine, ApiError> {
        self.engine.as_mut().ok_or_else(|| ApiError::new(503, format!("{NAME} models not loaded")))
    }

    fn demo_rows(&mut self) -> Result<Arc<Vec<DemoRow>>, ApiError> {
        if self.demo.rows.is_none() {
            let rows = read_demo_rows(&self.root.join("service/models/demo_stream.csv.gz")).map_err(|e| ApiError::new(500, format!("demo stream: {e}")))?;
            self.demo.rows = Some(Arc::new(rows));
        }
        Ok(self.demo.rows.clone().unwrap())
    }
}

// ---------------------------------------------------------------- responses

/// JSON written exactly as FastAPI writes it (`ensure_ascii=False`, compact separators, Python float repr).
pub struct PyJson(pub StatusCode, pub Value);

impl IntoResponse for PyJson {
    fn into_response(self) -> Response {
        let mut r = Response::new(Body::from(dumps(&self.1, RESPONSE_STYLE)));
        *r.status_mut() = self.0;
        r.headers_mut().insert("content-type", HeaderValue::from_static("application/json"));
        r
    }
}

fn ok(v: Value) -> Result<PyJson, ApiError> {
    Ok(PyJson(StatusCode::OK, v))
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub detail: Value,
}

impl ApiError {
    pub fn new(status: u16, detail: impl Into<String>) -> Self {
        ApiError { status, detail: Value::String(detail.into()) }
    }
    fn invalid(errs: Errors) -> Self {
        ApiError { status: 422, detail: Value::Array(errs.0) }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        PyJson(StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), json!({"detail": self.detail})).into_response()
    }
}

type ApiResult = Result<PyJson, ApiError>;

/// Parse a JSON body; FastAPI's errors for invalid JSON.
fn body_json(b: &Bytes) -> Result<Value, ApiError> {
    if b.is_empty() {
        return Err(ApiError { status: 422, detail: json!([{"type": "missing", "loc": ["body"], "msg": "Field required", "input": null}]) });
    }
    serde_json::from_slice(b).map_err(|e| ApiError {
        status: 422,
        detail: json!([{"type": "json_invalid", "loc": ["body", e.column()], "msg": "JSON decode error", "input": {}, "ctx": {"error": e.to_string()}}]),
    })
}

/// Parse and validate a JSON object body with `f`.
fn parse_obj<T>(b: &Bytes, f: impl FnOnce(&Obj, &mut Errors) -> T) -> Result<T, ApiError> {
    let v = body_json(b)?;
    let mut e = Errors::default();
    let out = Obj::new(&v, vec![], &mut e).map(|o| f(&o, &mut e));
    match out {
        Some(t) if e.is_empty() => Ok(t),
        _ => Err(ApiError::invalid(e)),
    }
}

fn bad_timestamp(field: &str, value: &str) -> ApiError {
    ApiError { status: 422, detail: json!([{"type": "value_error", "loc": ["body", field], "msg": "Value error, expected YYYY-MM-DD HH:MM (UTC)", "input": value}]) }
}

/// Run a handler body on the blocking pool with the state locked (requests are serialised, as in Python).
async fn with_state<F>(app: AppRef, f: F) -> ApiResult
where
    F: FnOnce(&mut AppState) -> ApiResult + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut st = app.state.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut st)
    })
    .await
    .unwrap_or_else(|e| Err(ApiError::new(500, format!("internal error: {e}"))))
}

// ---------------------------------------------------------------- router

pub fn build_app(app: AppRef) -> Router {
    let origins = std::env::var("CORS_ORIGINS").unwrap_or_else(|_| "*".into());
    let cors = if origins.trim() == "*" {
        tower_http::cors::CorsLayer::new().allow_origin(tower_http::cors::Any)
    } else {
        tower_http::cors::CorsLayer::new().allow_origin(origins.split(',').filter_map(|o| HeaderValue::from_str(o.trim()).ok()).collect::<Vec<_>>())
    }
    .allow_methods(tower_http::cors::Any)
    .allow_headers(tower_http::cors::Any);
    let api = Router::new()
        .route("/", get(health))
        .route("/health", get(health))
        .route("/v2/payment", post(v2_payment))
        .route("/v2/assess", post(v2_assess))
        .route("/v2/alerts", get(v2_alerts))
        .route("/v2/alerts/summary", get(v2_alert_summary))
        .route("/v2/alerts/{alert_id}/review", post(v2_review))
        .route("/v2/alerts/{alert_id}/evidence", get(v2_evidence))
        .route("/v2/alerts/{alert_id}/str", get(v2_str))
        .route("/v2/alerts/{alert_id}/customer", post(v2_customer))
        .route("/v2/alerts/{alert_id}/reverify", post(v2_reverify))
        .route("/v2/campaigns", get(v2_campaigns))
        .route("/v2/identity/check", post(v2_identity))
        .route("/v2/authenticate", post(v2_authenticate))
        .route("/v2/challenge/verify", post(v2_challenge_verify))
        .route("/v2/duress/register", post(v2_duress_register))
        .route("/v2/payments/confirm", post(v2_confirm))
        .route("/v2/behaviour/enrol", post(v2_behaviour_enrol))
        .route("/v2/behaviour/check", post(v2_behaviour_check))
        .route("/v2/consortium", get(v2_consortium))
        .route("/v2/consortium/check", post(v2_consortium_check))
        .route("/v2/consortium/publish", post(v2_consortium_publish))
        .route("/v2/keys/public", get(v2_public_key))
        .route("/v2/verify", post(v2_verify))
        .route("/v2/demo/analysts", get(v2_demo_analysts))
        .route("/v2/fixtures/mule", get(v2_fixture_mule))
        .route("/v2/audit", get(v2_audit))
        .route("/v2/message", post(v2_message))
        .route("/v2/message-model", get(v2_message_model))
        .route("/v2/payments/batch", post(v2_batch))
        .route("/v2/cases", get(v2_cases))
        .route("/v2/onboard", post(v2_onboard))
        .route("/v2/model-card", get(v2_model_card))
        .route("/v2/stats", get(v2_stats))
        .route("/v2/demo/reset", post(demo_reset))
        .route("/v2/demo/next", post(demo_next));
    Router::new()
        .route("/", get(|s| page(s, "index.html")))
        .route("/console", get(|s| page(s, "console.html")))
        .route("/investigate", get(|s| page(s, "investigate.html")))
        .route("/report", get(|s| page(s, "report.html")))
        .route("/favicon.svg", get(favicon))
        .route("/openapi.json", get(|| async { PyJson(StatusCode::OK, openapi::spec()) }))
        .route("/docs", get(|| async { axum::response::Html(openapi::DOCS_HTML) }))
        .nest("/api", api)
        .fallback(|| async { ApiError::new(404, "Not Found") })
        .method_not_allowed_fallback(|| async { ApiError::new(405, "Method Not Allowed") })
        .layer(middleware::from_fn(require_key))
        .layer(middleware::from_fn_with_state(app.clone(), rate_limit))
        .layer(middleware::from_fn(security_headers))
        .layer(cors)
        .with_state(app)
}

macro_rules! assets {
    ($($p:literal),* $(,)?) => { &[$(($p, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../", $p)) as &[u8])),*] };
}

/// Models, pages and fixtures compiled into the binary, so the server runs where only the binary is shipped
/// (Vercel's Rust framework mode builds and runs this binary without the repository files).
const ASSETS: &[(&str, &[u8])] = assets![
    "service/models/cascade_config.json", "service/models/l0_config.json", "service/models/l1_gatekeeper.json",
    "service/models/l2_xgb.json", "service/models/l2_lgbm.json", "service/models/l0_onboarding_lgbm.json",
    "service/models/message_model.json", "service/models/typing_verifier.json", "service/models/benchmark_results.json",
    "service/models/sample_applications.json", "service/models/demo_stream.csv.gz",
    "service/models/hardening.json", "service/models/hardening_xgb.json", "service/models/auth_config.json", "service/models/auth_xgb.json",
    "service/static/index.html", "service/static/console.html", "service/static/investigate.html", "service/static/report.html",
    "service/static/favicon.svg", "fixtures/mule_replay.json", "results/message_model_eval.json",
];

/// Secrets that must be set once the server runs with real analyst keys (not the public demo).
pub const PRODUCTION_SECRETS: [&str; 3] = ["RUSETECT_SIGNING_KEY", "RUSETECT_CONSORTIUM_SECRET", "RUSETECT_HASH_SALT"];

/// Fail closed. Without RUSETECT_ANALYST_KEYS the server would hand out the published demo keys, so that mode must be
/// asked for explicitly with RUSETECT_DEMO=1. With analyst keys the deployment is not a demo, so the hard-coded demo
/// consortium secret, demo hash salt and per-process signing key must not be used. Returns what is missing.
pub fn missing_production_secrets(env: impl Fn(&str) -> Option<String>) -> Vec<&'static str> {
    let Some(keys) = env("RUSETECT_ANALYST_KEYS") else {
        return if env("RUSETECT_DEMO").as_deref() == Some("1") { vec![] } else { vec!["RUSETECT_ANALYST_KEYS (or RUSETECT_DEMO=1 for the public demo)"] };
    };
    if let Err(e) = rusetect::security::parse_keys(&keys) {
        eprintln!("RUSETECT_ANALYST_KEYS: {e}");
        return vec!["a valid RUSETECT_ANALYST_KEYS ({key: {analyst, role}}, keys of 16+ characters)"];
    }
    PRODUCTION_SECRETS.into_iter().filter(|k| env(k).map(|v| v.trim().is_empty()).unwrap_or(true)).collect()
}

/// Start-up check for [`missing_production_secrets`] against the process environment.
pub fn check_config() -> std::io::Result<()> {
    let missing = missing_production_secrets(|k| std::env::var(k).ok());
    if missing.is_empty() {
        return Ok(());
    }
    Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,
        format!("refusing to start: {} not set", missing.join(", "))))
}

/// If no repository root with the models can be found, unpack the embedded copy to a temporary folder and point
/// RUSETECT_ROOT at it. Call once at start-up, before any other thread reads the environment.
pub fn ensure_root() -> std::io::Result<()> {
    if paths::repo_root().join("service/models/cascade_config.json").exists() {
        return Ok(());
    }
    let root = std::env::temp_dir().join("rusetect");
    for (path, bytes) in ASSETS {
        let dst = root.join(path);
        if dst.metadata().map(|m| m.len() == bytes.len() as u64).unwrap_or(false) {
            continue;
        }
        std::fs::create_dir_all(dst.parent().unwrap())?;
        std::fs::write(dst, bytes)?;
    }
    std::env::set_var("RUSETECT_ROOT", &root);
    Ok(())
}

pub fn new_shared() -> AppRef {
    let limit = std::env::var("RUSETECT_RATE_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(240);
    let state = AppState::load();
    let root = state.root.clone();
    Arc::new(Shared { state: Mutex::new(state), rate: Mutex::new(RateLimiter::new(limit, 10.0)), root })
}

/// Use X-Forwarded-For for client identity only behind a proxy that sets it: Vercel (VERCEL is set by the platform and
/// its edge overwrites the header) or RUSETECT_TRUST_PROXY=1.
static TRUST_PROXY: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
    std::env::var("RUSETECT_TRUST_PROXY").map(|v| v == "1").unwrap_or(false) || std::env::var("VERCEL").is_ok()
});

/// Accounts with a typing profile kept in memory (each holds at most 20 sessions of 1,000 key pairs).
const MAX_PROFILES: usize = 100_000;

/// Outside demo mode every /api route needs a known X-Analyst-Key except the public ones (security::gate).
async fn require_key(req: Request, next: Next) -> Response {
    if let Some(path) = req.uri().path().strip_prefix("/api") {
        let key = req.headers().get("x-analyst-key").and_then(|v| v.to_str().ok());
        if let Err((code, msg)) = rusetect::security::gate(&KEYS, req.method().as_str(), path, key) {
            return ApiError::new(code, msg).into_response();
        }
    }
    next.run(req).await
}

/// Baseline response headers: no MIME sniffing, no framing (clickjacking), no referrer leaks.
async fn security_headers(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    r
}

/// Requests per 10 s per client on POST /api.
async fn rate_limit(State(app): State<AppRef>, req: Request, next: Next) -> Response {
    if req.method() == Method::POST && req.uri().path().starts_with("/api/") {
        let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip().to_string()).unwrap_or_else(|| "?".into());
        // X-Forwarded-For is client controlled unless a proxy we trust overwrites it; trusting it unconditionally let any
        // caller pick a fresh identity per request and bypass the limit.
        let forwarded = if *TRUST_PROXY { req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok()) } else { None };
        let client = forwarded.map(String::from).unwrap_or(peer);
        let client = client.split(',').next().unwrap_or("").trim().to_string();
        if !app.rate.lock().unwrap_or_else(|p| p.into_inner()).allow(&client) {
            return PyJson(StatusCode::TOO_MANY_REQUESTS, json!({"detail": "rate limit: too many requests, slow down"})).into_response();
        }
    }
    next.run(req).await
}

// ---------------------------------------------------------------- pages

async fn page(State(app): State<AppRef>, name: &'static str) -> Response {
    let root = app.root.clone();
    match tokio::fs::read_to_string(root.join("service/static").join(name)).await {
        Ok(html) => axum::response::Html(html).into_response(),
        Err(_) => ApiError::new(404, "Not Found").into_response(),
    }
}

async fn favicon(State(app): State<AppRef>) -> Response {
    let root = app.root.clone();
    match tokio::fs::read(root.join("service/static/favicon.svg")).await {
        Ok(b) => ([("content-type", "image/svg+xml")], b).into_response(),
        Err(_) => ApiError::new(404, "Not Found").into_response(),
    }
}

async fn health(State(app): State<AppRef>) -> ApiResult {
    with_state(app, |s| {
        ok(json!({"service": format!("{NAME} API"), "name": NAME, "id": SLUG, "version": VERSION, "status": "ok", "engine_loaded": s.engine.is_some(),
                  "hardened": s.engine.as_ref().and_then(|e| e.models.as_ref()).map(|m| m.hardening.is_some()).unwrap_or(false),
                  "ruleset": ctx::RULESET_VERSION, "auth": if KEYS.demo_mode { "demo keys (set RUSETECT_ANALYST_KEYS)" } else { "configured keys" },
                  "signing": {"alg": "Ed25519", "key_id": s.signer.key_id, "ephemeral": s.signer.ephemeral}}))
    }).await
}

// ---------------------------------------------------------------- scoring

/// Account state is kept in memory, so random account ids could exhaust it. Past this many accounts a payment that
/// would add a new account is refused (503); known accounts still score. RUSETECT_MAX_ACCOUNTS overrides the default.
pub static MAX_ACCOUNTS: std::sync::LazyLock<usize> =
    std::sync::LazyLock::new(|| std::env::var("RUSETECT_MAX_ACCOUNTS").ok().and_then(|v| v.parse().ok()).unwrap_or(2_000_000));

fn score_with_context(s: &mut AppState, p: &Payment) -> Result<Map<String, Value>, ApiError> {
    let eng = s.need_engine()?;
    if eng.n_accounts() >= *MAX_ACCOUNTS && !(eng.knows(&p.from_bank, &p.from_account) && eng.knows(&p.to_bank, &p.to_account)) {
        return Err(ApiError::new(503, "account state is full; reset the demo or raise RUSETECT_MAX_ACCOUNTS"));
    }
    let mut res = eng.score_payment(p);
    s.lures.apply_to_payment(p, &mut res);
    Ok(res)
}

fn payment_of(p: &PaymentIn) -> Result<Payment, ApiError> {
    let d = p.to_payment().ok_or_else(|| bad_timestamp("timestamp", &p.timestamp))?;
    // a far-future minute would make every window evict relative to it and poison the account's state
    if d.tmin > (rusetect::pyfmt::time_time() / 60.0) as i64 + 1440 {
        return Err(ApiError::new(422, "timestamp is more than one day in the future"));
    }
    Ok(d)
}

/// Score one payment and update account state. Send payments in time order. A recent risky message to the payer,
/// or a payee named in one, can raise the decision (never lower it).
async fn v2_payment(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let pin = parse_obj(&b, PaymentIn::parse)?;
    let p = payment_of(&pin)?;
    with_state(app, move |s| ok(Value::Object(score_with_context(s, &p)?))).await
}

/// Payment cascade + context layer (ruleset in the response). The final decision is the stricter of the two;
/// context alone never goes beyond an analyst hold. Message text is redacted before it is returned or stored.
async fn v2_assess(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let a = parse_obj(&b, AssessIn::parse)?;
    let d = payment_of(&a.payment)?;
    with_state(app, move |s| assess(s, &a, &d)).await
}

fn assess(s: &mut AppState, a: &AssessIn, d: &Payment) -> ApiResult {
    let t0 = Instant::now();
    s.need_engine()?;
    let (sender, payee) = (d.sender_key(), d.payee_key());
    let sf = s.engine.as_ref().unwrap().sender_fan_in_24h(d);
    let flagged_payers = s.coord.flagged_payers(&payee, d.tmin);
    let shared = s.consortium.check("mule_account", &payee);
    let shared_flag = shared["flagged"].as_bool().unwrap_or(false);
    let pay = score_with_context(s, d)?;
    let msg = a.message.as_deref().filter(|m| !m.is_empty());
    let mm = match (a.consent, msg, s.messages.as_ref()) {
        (true, Some(m), Some(model)) => Some(model.score(m)),
        _ => None,
    };
    let band = mm.as_ref().filter(|_| a.use_intent_model).and_then(|m| m["band"].as_str().map(String::from));
    let feats = ctx::features_from_json(pay["features"].as_object().unwrap());
    let mut pc = ctx::from_engine_features(&feats, sf);
    pc.coordinated = (s.coord.enabled && flagged_payers >= s.coord.k) || shared_flag;
    let mut ss = a.session.as_ref().map(|x| x.to_signals());
    if let Some(x) = ss.as_mut() {
        x.network_flag = shared_flag;
    }
    let sres = assess_session(ss.as_ref());
    let c = ctx::assess(a.message.as_deref(), &pc, a.consent, None, true, band.as_deref(), sres.as_ref());
    let decision = pay["decision"].as_str().unwrap_or("Approve").to_string();
    let comb = ctx::combine(&decision, &c);
    let final_decision = comb["final_decision"].as_str().unwrap().to_string();
    s.coord.record(&sender, &payee, d.tmin, ctx::payment_rank(&final_decision));
    let mut out = Map::new();
    let mut pv = Map::new();
    for k in ["l1_score", "layer_exit", "risk", "decision", "reasons", "case_id", "latency_ms"] {
        pv.insert(k.into(), pay.get(k).cloned().unwrap_or(Value::Null));
    }
    out.insert("payment".into(), Value::Object(pv));
    out.insert("context".into(), c.to_json());
    for (k, v) in comb {
        out.insert(k, v);
    }
    out.insert("network".into(), json!({"flagged_payers_in_window": flagged_payers, "local_rule_enabled": s.coord.enabled,
                                        "flagged_by_consortium": shared["banks"]}));
    out.insert("message_model".into(), match &mm {
        Some(m) => json!({"band": m["band"], "workflow": m["workflow"]}),
        None => Value::Null,
    });
    let cfg = s.engine.as_ref().unwrap().cfg.raw.clone();
    out.insert("risk_score".into(), rusetect::risk_score::display_score(&final_decision, pay.get("risk").and_then(|v| v.as_f64()), c.total, &cfg));
    let wf = mm.as_ref().and_then(|m| m["workflow"].get("id")).and_then(|v| v.as_str());
    let identity = sres.as_ref().map(|x| &x.identity);
    out.insert("intervention".into(), plan(&final_decision, &c.reasons, identity, wf));
    let inputs = a.dump_without_message();
    let alert = s.desk.open_alert(&inputs, &Value::Object(out.clone()));
    let aid = alert.as_ref().map(|x| x["alert_id"].clone()).unwrap_or(Value::Null);
    out.insert("alert_id".into(), aid.clone());
    out.insert("shown_decision".into(), alert.as_ref().map(|x| x["shown_decision"].clone()).unwrap_or(json!("Approve")));
    out.insert("time_to_decision_ms".into(), jf(py_round(t0.elapsed().as_secs_f64() * 1000.0, 2)));
    let core = json!({"alert_id": aid, "final_decision": final_decision, "shown_decision": out["shown_decision"], "risk_score": out["risk_score"]["score"],
                      "ruleset": c.ruleset, "inputs": inputs});
    let mut sig = s.signer.sign(&core);
    sig["signed_fields"] = core;
    out.insert("signature".into(), sig);
    let out = Value::Object(out);
    if let Some(id) = aid.as_str() {
        s.desk.set_assessed(id, out.clone()); // the stored alert holds the finished result, as in Python
    }
    ok(out)
}

// ---------------------------------------------------------------- review desk

#[derive(serde::Deserialize)]
struct StatusQ {
    status: Option<String>,
}

/// Alerts awaiting or after human review, newest first.
async fn v2_alerts(State(app): State<AppRef>, h: HeaderMap, Query(q): Query<StatusQ>) -> ApiResult {
    require_reader(&h)?;
    with_state(app, move |s| {
        let al: Vec<Value> = s.desk.alerts.values().filter(|a| q.status.as_deref().map(|st| a["status"] == st).unwrap_or(true)).rev().cloned().collect();
        ok(Value::Array(al))
    }).await
}

/// Alerts, evidence packs, STR drafts, cases and the audit log name accounts and suspicions (sharing an STR is tipping
/// off). With configured keys they need a valid X-Analyst-Key of any role; demo mode keeps them open for the public demo.
fn require_reader(h: &HeaderMap) -> Result<(), ApiError> {
    if KEYS.demo_mode || analyst_for(header(h, "x-analyst-key").as_deref()).is_some() {
        Ok(())
    } else {
        Err(ApiError::new(401, "missing or unknown X-Analyst-Key"))
    }
}

fn header(h: &HeaderMap, k: &str) -> Option<String> {
    h.get(k).and_then(|v| v.to_str().ok()).map(String::from)
}

fn review_err(e: ReviewError) -> ApiError {
    match e {
        ReviewError::NotFound => ApiError::new(404, "unknown alert"),
        ReviewError::Invalid(m) => ApiError::new(400, m),
    }
}

/// Analyst action. Needs X-Analyst-Key (who you are) and a fresh X-Nonce (no replays). Two different analysts must
/// approve a block; the block then publishes the payee to the consortium mule registry (hashed).
async fn v2_review(State(app): State<AppRef>, Path(alert_id): Path<String>, h: HeaderMap, b: Bytes) -> ApiResult {
    let (action, note) = parse_obj(&b, |o, e| {
        let action = o.req_str(e, "action");
        let note = o.str_or(e, "note", "");
        o.check_len(e, "note", &note, None, Some(500));
        let _ = o.opt_str(e, "analyst");
        (action, note)
    })?;
    let (key, nonce) = (header(&h, "x-analyst-key"), header(&h, "x-nonce"));
    with_state(app, move |s| {
        let who = analyst_for(key.as_deref()).ok_or_else(|| ApiError::new(401, "missing or unknown X-Analyst-Key"))?;
        if !allowed(&who.role, &action) {
            return Err(ApiError::new(403, format!("role {} cannot {}", who.role, action)));
        }
        if !s.nonces.use_nonce(nonce.as_deref()) {
            return Err(ApiError::new(409, "X-Nonce missing or already used"));
        }
        let a = s.desk.act(&alert_id, &who.analyst, &action, &note).map_err(review_err)?;
        if a["status"] == "blocked" && a.get("published").map(|p| p.is_null() || p == &json!(false) || p == &json!(0)).unwrap_or(true) {
            let p = &a["payment"];
            let ident = format!("{}:{}", p["to_bank"].as_str().unwrap_or(""), p["to_account"].as_str().unwrap_or(""));
            let pack_hash = s.desk.evidence(&alert_id).and_then(|e| e["pack_hash"].as_str().map(String::from));
            let e = s.consortium.publish(THIS_BANK, "mule_account", &ident, "block approved by two analysts", pack_hash.as_deref()).map_err(|m| ApiError::new(400, m))?;
            s.desk.set_field(&alert_id, "published", e["seq"].clone());
            s.desk.system_event(&alert_id, "consortium_published", "system", &format!("ledger entry {}", e["seq"]), false).map_err(review_err)?;
        }
        ok(s.desk.alerts[&alert_id].clone())
    }).await
}

async fn v2_evidence(State(app): State<AppRef>, h: HeaderMap, Path(alert_id): Path<String>) -> ApiResult {
    require_reader(&h)?;
    with_state(app, move |s| {
        let mut pack = s.desk.evidence(&alert_id).ok_or_else(|| ApiError::new(404, "unknown alert"))?;
        pack["signature"] = s.signer.sign(&json!({"pack_hash": pack["pack_hash"], "alert_id": alert_id}));
        ok(pack)
    }).await
}

/// STR draft built from the evidence pack (template, not a language model). For analyst review; never filed automatically.
async fn v2_str(State(app): State<AppRef>, h: HeaderMap, Path(alert_id): Path<String>) -> ApiResult {
    require_reader(&h)?;
    with_state(app, move |s| {
        let pack = s.desk.evidence(&alert_id).ok_or_else(|| ApiError::new(404, "unknown alert"))?;
        let camps = rusetect::campaigns::build_recent(&s.desk.alerts);
        let camp = camps.iter().find(|c| c["alerts"].as_array().map(|a| a.iter().any(|x| x == alert_id.as_str())).unwrap_or(false));
        ok(rusetect::str_report::draft(&pack, camp, rusetect::str_report::DEFAULT_ENTITY))
    }).await
}

/// Alerts linked into scam campaigns by shared payee, payee identifiers in messages, or near identical scripts.
async fn v2_campaigns(State(app): State<AppRef>) -> ApiResult {
    with_state(app, |s| {
        ok(Value::Array(rusetect::campaigns::build_recent(&s.desk.alerts)))
    }).await
}

/// The customer's answer on the pause screen.
async fn v2_customer(State(app): State<AppRef>, Path(alert_id): Path<String>, b: Bytes) -> ApiResult {
    let answer = parse_obj(&b, |o, e| o.req_str(e, "answer"))?;
    with_state(app, move |s| {
        if !s.desk.alerts.contains_key(&alert_id) {
            return Err(ApiError::new(404, "unknown alert"));
        }
        if answer != "stop" && answer != "continue" {
            return Err(ApiError::new(400, "answer must be stop or continue"));
        }
        // the pause screen answers an alert no analyst has acted on; a hold, block, release or verification stands
        if !matches!(s.desk.alerts[&alert_id]["status"].as_str(), Some("open" | "cooling_off" | "stopped_by_customer")) {
            return Err(ApiError::new(409, "an analyst has acted on this alert; the customer answer cannot change it"));
        }
        let status = if answer == "stop" { "stopped_by_customer" } else { "cooling_off" };
        ok(s.desk.system_event(&alert_id, status, "customer", &format!("pause screen answer: {answer}"), true).map_err(review_err)?)
    }).await
}

/// Customer recovery: a re-verified customer goes back to an analyst for release.
async fn v2_reverify(State(app): State<AppRef>, Path(alert_id): Path<String>, b: Bytes) -> ApiResult {
    let method = parse_obj(&b, |o, e| o.str_or(e, "method", "video_kyc"))?;
    with_state(app, move |s| {
        if !s.desk.alerts.contains_key(&alert_id) {
            return Err(ApiError::new(404, "unknown alert"));
        }
        ok(s.desk.system_event(&alert_id, "customer_reverified", "customer", &format!("re-verified by {method}"), true).map_err(review_err)?)
    }).await
}

/// Run before the OTP is sent: unenrolled device plus a recent SIM swap means no OTP and re-verification.
async fn v2_identity(b: Bytes) -> ApiResult {
    let si = parse_obj(&b, SessionIn::parse)?;
    let res = identity_check(&si.to_signals());
    let mut out = res.as_object().cloned().unwrap_or_default();
    out.insert("intervention".into(), plan("Approve", &[], Some(&res), None));
    ok(Value::Object(out))
}

/// Account-takeover check for a login or payment (PRAMAAN): allow, challenge (and which one) or block, with reasons in
/// English and Hindi and exact tree-SHAP signals. Body: user_id, ts (epoch seconds) and optional device, network,
/// behaviour and transaction objects; see crates/rusetect/src/pramaan.rs.
async fn v2_authenticate(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let req: Value = serde_json::from_slice(&b).map_err(|_| ApiError::new(422, "body must be a JSON object"))?;
    let o = req.as_object().ok_or_else(|| ApiError::new(422, "body must be a JSON object"))?;
    if !o.get("user_id").and_then(|v| v.as_str()).is_some_and(|u| !u.is_empty() && u.chars().count() <= 128) {
        return Err(ApiError::new(422, "user_id: a string of 1 to 128 characters is required"));
    }
    if !o.get("ts").and_then(|v| v.as_f64()).is_some_and(|t| t.is_finite() && t > 0.0 && t < 4.0e9) {
        return Err(ApiError::new(422, "ts: epoch seconds are required"));
    }
    for k in ["device", "network", "behaviour", "transaction"] {
        if o.get(k).is_some_and(|v| !v.is_object() && !v.is_null()) {
            return Err(ApiError::new(422, format!("{k}: must be an object")));
        }
    }
    let id = o.get("request_id").and_then(|v| v.as_str()).filter(|r| !r.is_empty() && r.len() <= 64).map(String::from)
        .unwrap_or_else(|| (0..16).map(|_| format!("{:x}", rand::random::<u8>() % 16)).collect());
    with_state(app, move |s| {
        let mut req = req;
        // payee intelligence from RuseTect: a payee other banks flagged as a mule on the consortium ledger gets the
        // risk PRAMAAN gives a held mule account (0.9), unless the caller already sent a payee risk
        let mut flagged_by = Value::Null;
        if let Some(tx) = req.get_mut("transaction").and_then(|t| t.as_object_mut()) {
            if tx.get("payee_risk").is_none_or(|v| v.is_null()) {
                if let Some(pid) = tx.get("payee_id").and_then(|v| v.as_str()).filter(|p| !p.is_empty()) {
                    let c = s.consortium.check("mule_account", pid);
                    if c["flagged"] == true {
                        tx.insert("payee_risk".into(), json!(0.9));
                        flagged_by = c["banks"].clone();
                    }
                }
            }
        }
        let a = s.auth.as_mut().ok_or_else(|| ApiError::new(503, "authentication model not loaded"))?;
        let mut out = a.authenticate(&req, id);
        out["payee_flagged_by_consortium"] = flagged_by;
        ok(out)
    }).await
}

/// The outcome of the challenge a decision asked for: passed -> allow (a passkey or bank call-back also trusts the
/// device); failed -> block, and the account's next attempts are blocked for 30 minutes.
async fn v2_challenge_verify(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let (id, passed) = parse_obj(&b, |o, e| (o.req_str(e, "decision_id"), o.bool_or(e, "passed", false)))?;
    with_state(app, move |s| {
        let a = s.auth.as_mut().ok_or_else(|| ApiError::new(503, "authentication model not loaded"))?;
        a.verify_challenge(&id, passed).map(ok).unwrap_or_else(|| Err(ApiError::new(404, "no pending challenge with this decision_id")))
    }).await
}

async fn v2_duress_register(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let (account, normal, duress, contacts) = parse_obj(&b, |o, e| {
        (o.req_str(e, "account"), o.req_str(e, "normal_pin"), o.req_str(e, "duress_pin"), o.str_list(e, "trusted_contacts"))
    })?;
    // PBKDF2 runs outside the shared lock, so PIN routes cannot stall every other request
    let u = tokio::task::spawn_blocking(move || rusetect::intervention::DuressVault::prepare(&normal, &duress, &contacts)).await
        .map_err(|e| ApiError::new(500, format!("internal error: {e}")))?.map_err(|m| ApiError::new(400, m))?;
    with_state(app, move |s| s.duress.insert(&account, u).map(|v| PyJson(StatusCode::OK, v)).map_err(|m| ApiError::new(409, m))).await
}

/// Confirm a payment with a PIN. The duress PIN shows success to the customer but holds the money and alerts trusted
/// contacts and the fraud desk. The response says what the customer sees and what really happened.
async fn v2_confirm(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let (account, pin, alert_id) = parse_obj(&b, |o, e| (o.req_str(e, "account"), o.req_str(e, "pin"), o.opt_str(e, "alert_id")))?;
    let acct = account.clone();
    let salt = with_state(app.clone(), move |s| ok(json!(s.duress.salt(&acct).map(|b| b.to_vec())))).await?;
    let h = match serde_json::from_value::<Option<Vec<u8>>>(salt.1.clone()).ok().flatten() {
        Some(salt) => Some(tokio::task::spawn_blocking(move || rusetect::intervention::pin_hash(&pin, &salt)).await
            .map_err(|e| ApiError::new(500, format!("internal error: {e}")))?),
        None => None,
    };
    with_state(app, move |s| {
        let r = match h {
            Some(h) => s.duress.confirm_hashed(&account, &h),
            None => json!({"result": "not_registered"}),
        };
        if r["result"] == "duress" {
            if let Some(aid) = alert_id.filter(|a| !a.is_empty() && s.desk.alerts.contains_key(a)) {
                let _ = s.desk.system_event(&aid, "duress_hold", "customer", "duress PIN used: funds held, trusted contacts alerted", true);
            }
        }
        ok(r)
    }).await
}

fn digraphs_of(m: &Map<String, Value>) -> Result<IndexMap<String, f64>, ApiError> {
    m.iter().map(|(k, v)| {
        rusetect::pyfmt::py_float_value(v).filter(|x| x.is_finite()).map(|x| (k.clone(), x)).ok_or_else(|| ApiError {
            status: 422,
            detail: json!([{"type": "float_parsing", "loc": ["body", "digraphs", k], "msg": "Input should be a valid number", "input": v}]),
        })
    }).collect()
}

async fn v2_behaviour_enrol(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let (account, dg) = parse_obj(&b, |o, e| (o.req_str(e, "account"), o.req_dict(e, "digraphs")))?;
    let dg = digraphs_of(&dg)?;
    with_state(app, move |s| {
        if s.profiles.len() >= MAX_PROFILES && !s.profiles.contains_key(&account) {
            return Err(ApiError::new(503, "typing profiles are full"));
        }
        let prof = s.profiles.entry(account.clone()).or_default();
        prof.enrol(dg);
        ok(json!({"account": account, "sessions": prof.sessions.len(), "ready": prof.ready()}))
    }).await
}

/// Typing rhythm check (verifier trained and tested on real keystrokes); use the result as SessionIn.typing_anomaly.
async fn v2_behaviour_check(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let (account, dg) = parse_obj(&b, |o, e| (o.req_str(e, "account"), o.req_dict(e, "digraphs")))?;
    let dg = digraphs_of(&dg)?;
    with_state(app, move |s| {
        ok(match s.profiles.get(&account) {
            Some(p) => p.check(&dg, s.verifier.as_ref()),
            None => json!({"ready": false, "score": null, "anomaly": false}),
        })
    }).await
}

/// Signed, hash chained cross bank ledger; identifiers are HMAC tokens.
async fn v2_consortium(State(app): State<AppRef>) -> ApiResult {
    with_state(app, |s| {
        let members: Map<String, Value> = s.consortium.banks.iter().map(|(b, k)| (b.clone(), json!(k.public_b64))).collect();
        ok(json!({"members": members, "entries": s.consortium.ledger, "valid": s.consortium.verify(), "merkle_root": s.consortium.merkle_root()}))
    }).await
}

async fn v2_consortium_check(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let (registry, identifier) = parse_obj(&b, |o, e| (o.str_or(e, "registry", "mule_account"), o.req_str(e, "identifier")))?;
    with_state(app, move |s| ok(s.consortium.check(&registry, &identifier))).await
}

/// Demo: a member bank publishes a flag (needs an analyst key).
async fn v2_consortium_publish(State(app): State<AppRef>, h: HeaderMap, b: Bytes) -> ApiResult {
    let (bank, registry, identifier, reason) = parse_obj(&b, |o, e| {
        (o.str_or(e, "bank", "BANK-B"), o.str_or(e, "registry", "mule_account"), o.req_str(e, "identifier"), o.str_or(e, "reason", "confirmed mule"))
    })?;
    let key = header(&h, "x-analyst-key");
    with_state(app, move |s| {
        let who = analyst_for(key.as_deref()).ok_or_else(|| ApiError::new(401, "missing or unknown X-Analyst-Key"))?;
        if !matches!(who.role.as_str(), "analyst" | "supervisor") {
            return Err(ApiError::new(403, "publishing to the consortium needs an analyst or supervisor key"));
        }
        // a real deployment signs only as itself; the demo lets the visitor play the other member banks
        let bank = if KEYS.demo_mode { bank } else { THIS_BANK.to_string() };
        if !s.consortium.banks.contains_key(&bank) {
            return Err(ApiError::new(400, "unknown member bank"));
        }
        s.consortium.publish(&bank, &registry, &identifier, &reason, None).map(|v| PyJson(StatusCode::OK, v)).map_err(|m| ApiError::new(400, m))
    }).await
}

async fn v2_public_key(State(app): State<AppRef>) -> ApiResult {
    with_state(app, |s| ok(json!({"alg": "Ed25519", "key_id": s.signer.key_id, "public_key_b64": s.signer.public_b64, "ephemeral": s.signer.ephemeral}))).await
}

async fn v2_verify(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let (fields, sig) = parse_obj(&b, |o, e| (o.req_dict(e, "signed_fields"), o.req_str(e, "signature")))?;
    with_state(app, move |s| ok(json!({"valid": Signer::verify(&Value::Object(fields), &sig, &s.signer.public_b64)}))).await
}

/// Demo keys (only when RUSETECT_ANALYST_KEYS is not set).
async fn v2_demo_analysts() -> ApiResult {
    if !KEYS.demo_mode {
        return ok(json!([]));
    }
    ok(Value::Array(KEYS.keys.iter().map(|(k, a)| json!({"key": k, "analyst": a.analyst, "role": a.role})).collect()))
}

fn read_json(path: PathBuf) -> Result<Value, ApiError> {
    let t = std::fs::read_to_string(&path).map_err(|e| ApiError::new(500, format!("{}: {e}", path.display())))?;
    serde_json::from_str(&t).map_err(|e| ApiError::new(500, format!("{}: {e}", path.display())))
}

/// The fictional mule replay fixture used by the investigator demo.
async fn v2_fixture_mule(State(app): State<AppRef>) -> ApiResult {
    with_state(app, |s| ok(read_json(s.root.join("fixtures/mule_replay.json"))?)).await
}

async fn v2_audit(State(app): State<AppRef>, h: HeaderMap) -> ApiResult {
    require_reader(&h)?;
    with_state(app, |s| ok(json!({"entries": s.desk.log, "chain_valid": s.desk.verify_chain()}))).await
}

/// L-1 lure detector. Scores the text, explains which scam workflow and tactics it matches, and applies the
/// alert-fatigue policy. Entities in risky messages join a payee watchlist.
async fn v2_message(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let (text, channel, ts, bank, account) = parse_obj(&b, |o, e| {
        let text = o.req_str(e, "text");
        if o.get("text").map(|v| v.is_string()).unwrap_or(false) {
            o.check_len(e, "text", &text, Some(1), Some(5000));
        }
        (text, o.str_or(e, "channel", "sms"), o.opt_str(e, "timestamp"), o.opt_str(e, "bank"), o.opt_str(e, "account"))
    })?;
    let tmin = match &ts {
        Some(t) => rusetect::pyfmt::parse_minutes(&t.replace('/', "-")).ok_or_else(|| bad_timestamp("timestamp", t))?,
        None => chrono::Utc::now().timestamp().div_euclid(60),
    };
    with_state(app, move |s| {
        let model = s.messages.as_ref().ok_or_else(|| ApiError::new(503, "Message model not loaded"))?;
        let mut res = model.score(&text);
        let acct = account.filter(|a| !a.is_empty()).map(|a| format!("{}:{}", bank.as_deref().unwrap_or("None"), a));
        res["alert"] = s.lures.record_message(acct.as_deref(), tmin, &res);
        res["channel"] = json!(channel);
        ok(res)
    }).await
}

/// How many messages were scored, pushed, or suppressed (and why); how many payments a lure escalated.
async fn v2_alert_summary(State(app): State<AppRef>) -> ApiResult {
    with_state(app, |s| ok(s.lures.summary())).await
}

async fn v2_message_model(State(app): State<AppRef>) -> ApiResult {
    with_state(app, |s| {
        let p = s.root.join("results/message_model_eval.json");
        if p.exists() { ok(read_json(p)?) } else { ok(json!({})) }
    }).await
}

async fn v2_batch(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let v = body_json(&b)?;
    let mut e = Errors::default();
    let Some(arr) = v.as_array() else {
        e.push("list_type", &[], "Input should be a valid list", &v);
        return Err(ApiError::invalid(e));
    };
    let items: Vec<PaymentIn> = arr.iter().enumerate().filter_map(|(i, x)| Obj::new(x, vec![i.to_string()], &mut e).map(|o| PaymentIn::parse(&o, &mut e))).collect();
    if !e.is_empty() {
        return Err(ApiError::invalid(e));
    }
    if items.len() > 20000 {
        return Err(ApiError::new(413, "Batch too large (max 20000)"));
    }
    // in time order by the parsed minute (sorting the raw strings misorders mixed separators or unpadded months)
    let mut pays: Vec<Payment> = items.iter().map(payment_of).collect::<Result<_, _>>()?;
    pays.sort_by_key(|p| p.tmin);
    with_state(app, move |s| {
        s.need_engine()?;
        // all or nothing: refuse up front rather than fail half way (a retry would then count the first half twice)
        if s.engine.as_ref().unwrap().n_accounts() + 2 * pays.len() > *MAX_ACCOUNTS {
            return Err(ApiError::new(503, "account state is too full for this batch; reset the demo or raise RUSETECT_MAX_ACCOUNTS"));
        }
        let mut out = Vec::with_capacity(pays.len());
        for p in &pays {
            out.push(score_with_context(s, p)?);
        }
        let mut flagged: Vec<Value> = out.iter().enumerate().filter(|(_, r)| r["band"] != "safe")
            .map(|(i, r)| json!({"i": i, "risk": r["risk"], "decision": r["decision"], "case_id": r["case_id"], "reasons": r["reasons"]})).collect();
        flagged.sort_by(|a, b| b["risk"].as_f64().partial_cmp(&a["risk"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
        flagged.truncate(500);
        let l1 = out.iter().filter(|r| r["layer_exit"] == "L1").count();
        let l2 = out.iter().filter(|r| r["layer_exit"] == "L2").count();
        ok(json!({"scored": out.len(), "layer_exit": {"L1": l1, "L2": l2}, "flagged": flagged, "cases": s.engine.as_ref().unwrap().cases(20)}))
    }).await
}

#[derive(serde::Deserialize)]
struct LimitQ {
    limit: Option<String>,
}

fn int_query(v: &Option<String>, name: &str, default: i64) -> Result<i64, ApiError> {
    match v {
        None => Ok(default),
        Some(s) => s.trim().parse::<i64>().map_err(|_| ApiError {
            status: 422,
            detail: json!([{"type": "int_parsing", "loc": ["query", name], "msg": "Input should be a valid integer, unable to parse string as an integer", "input": s}]),
        }),
    }
}

async fn v2_cases(State(app): State<AppRef>, h: HeaderMap, Query(q): Query<LimitQ>) -> ApiResult {
    require_reader(&h)?;
    let limit = int_query(&q.limit, "limit", 50)?.clamp(1, 500) as usize;
    with_state(app, move |s| ok(Value::Array(s.need_engine()?.cases(limit)))).await
}

/// Account-opening application in the Feedzai BAF schema; omitted fields are treated as missing.
async fn v2_onboard(State(app): State<AppRef>, b: Bytes) -> ApiResult {
    let mut appl = parse_obj(&b, |o, e| {
        let age = o.req_int(e, "customer_age");
        let mut m = o.m.clone();
        m.insert("customer_age".into(), json!(age));
        m
    })?;
    // model_dump(): the declared field first, extra fields after, in the order sent
    let age = appl.shift_remove("customer_age").unwrap();
    let mut dumped = Map::new();
    dumped.insert("customer_age".into(), age);
    dumped.extend(appl);
    with_state(app, move |s| {
        let r = s.need_engine()?.score_application(&dumped).map_err(|e| ApiError::new(422, e.to_string()))?;
        ok(r)
    }).await
}

async fn v2_model_card(State(app): State<AppRef>) -> ApiResult {
    with_state(app, |s| ok(read_json(s.root.join("service/models/benchmark_results.json"))?)).await
}

async fn v2_stats(State(app): State<AppRef>) -> ApiResult {
    with_state(app, |s| {
        let e = s.need_engine()?;
        ok(json!({"payments_seen": e.n_seen, "accounts_tracked": e.n_accounts(), "decisions": e.decisions, "open_cases": e.case_alerts.len()}))
    }).await
}

// ---------------------------------------------------------------- demo replay

/// All payments before the demo window from the full IBM HI-Small CSV(s), if RUSETECT_HISTORY_GLOB is set.
pub fn full_history_rows() -> anyhow::Result<Option<Vec<Payment>>> {
    let Ok(pattern) = std::env::var("RUSETECT_HISTORY_GLOB") else { return Ok(None) };
    if pattern.is_empty() {
        return Ok(None);
    }
    let mut files: Vec<PathBuf> = glob::glob(&pattern)?.filter_map(|p| p.ok()).collect();
    files.sort();
    if files.is_empty() {
        return Ok(None);
    }
    let mut rows: Vec<(i64, usize, Payment)> = Vec::new();
    for f in files {
        let fh = std::fs::File::open(&f)?;
        let reader: Box<dyn std::io::Read> = if f.extension().map(|e| e == "gz").unwrap_or(false) { Box::new(flate2::read::MultiGzDecoder::new(fh)) } else { Box::new(fh) };
        let mut rd = csv::ReaderBuilder::new().has_headers(true).from_reader(std::io::BufReader::new(reader));
        for r in rd.records() {
            let r = r?;
            let g = |i: usize| r.get(i).unwrap_or("").to_string();
            let Some(tmin) = rusetect::pyfmt::parse_minutes_slash(&g(0)) else { continue };
            let rid = rows.len();
            rows.push((tmin, rid, Payment { from_bank: g(1), from_account: g(2), to_bank: g(3), to_account: g(4),
                amount_paid: rusetect::pyfmt::py_float_str(&g(7)).unwrap_or(0.0), payment_currency: g(8), receiving_currency: g(6),
                payment_format: g(9), tmin, eid: None }));
        }
    }
    rows.sort_by_key(|r| (r.0, r.1));
    let keep = (0.8 * rows.len() as f64) as usize;
    Ok(Some(rows.into_iter().take(keep).map(|r| r.2).collect()))
}

#[derive(serde::Deserialize)]
struct WarmQ {
    warm: Option<String>,
}

/// Clear state, then ingest earlier payments as account history (features only, no scoring).
async fn demo_reset(State(app): State<AppRef>, Query(q): Query<WarmQ>) -> ApiResult {
    let warm = match &q.warm {
        None => true,
        Some(v) => validate::parse_bool_str(v).ok_or_else(|| ApiError {
            status: 422,
            detail: json!([{"type": "bool_parsing", "loc": ["query", "warm"], "msg": "Input should be a valid boolean, unable to interpret input", "input": v}]),
        })?,
    };
    with_state(app, move |s| {
        s.need_engine()?.reset();
        s.lures.reset();
        s.desk.reset();
        s.coord.reset();
        s.duress.reset();
        s.consortium.reset();
        s.profiles.clear();
        if let Some(a) = s.auth.as_mut() {
            a.state.clear();
        }
        let rows = s.demo_rows()?;
        let t = Instant::now();
        let mut source = "bundled sample";
        let hist = if warm { full_history_rows().map_err(|e| ApiError::new(500, format!("history: {e}")))? } else { None };
        let eng = s.engine.as_mut().unwrap();
        let n = match hist {
            Some(h) => {
                for p in &h {
                    eng.features(p);
                }
                source = "full IBM history";
                h.len()
            }
            None => {
                let n = if warm { DEMO_WARM.min(rows.len()) } else { 0 };
                for r in &rows[..n] {
                    if let Some(p) = Payment::from_demo_row(r) {
                        eng.features(&p);
                    }
                }
                n
            }
        };
        s.demo.ptr = DEMO_WARM;
        s.demo.warm = DEMO_WARM;
        s.demo.truth = truth0();
        s.demo.feed.clear();
        ok(json!({"history_ingested": n, "source": source, "seconds": py_round(t.elapsed().as_secs_f64(), 2), "remaining": rows.len() as i64 - DEMO_WARM as i64}))
    }).await
}

#[derive(serde::Deserialize)]
struct NQ {
    n: Option<String>,
}

/// Score the next n demo payments. Labels are only used to report accuracy, never by the model.
async fn demo_next(State(app): State<AppRef>, Query(q): Query<NQ>) -> ApiResult {
    let n = int_query(&q.n, "n", 2000)?.clamp(1, 20000) as usize;
    with_state(app, move |s| {
        s.need_engine()?;
        let rows = s.demo_rows()?;
        if s.demo.ptr < DEMO_WARM {
            return Err(ApiError::new(409, "Load account history first (POST /api/v2/demo/reset)"));
        }
        let start = s.demo.ptr.min(rows.len());
        let end = (start + n).min(rows.len());
        let batch = &rows[start..end];
        s.demo.ptr += batch.len();
        let mut exits: IndexMap<&str, i64> = [("L1", 0), ("L2", 0)].into_iter().collect();
        let mut bands: IndexMap<&str, i64> = [("safe", 0), ("med", 0), ("high", 0)].into_iter().collect();
        let mut lat = Vec::with_capacity(batch.len());
        let mut rng = rand::thread_rng();
        for r in batch {
            let Some(p) = Payment::from_demo_row(r) else { continue };
            let res = s.engine.as_mut().unwrap().score_payment(&p);
            let y = r.y == "1";
            let band = res["band"].as_str().unwrap_or("safe").to_string();
            *exits.entry(if res["layer_exit"] == "L1" { "L1" } else { "L2" }).or_insert(0) += 1;
            *bands.entry(match band.as_str() { "high" => "high", "med" => "med", _ => "safe" }).or_insert(0) += 1;
            lat.push(res["latency_ms"].as_f64().unwrap_or(0.0));
            let blocked = band == "high";
            let t = &mut s.demo.truth;
            t["pos"] += y as i64;
            t["tp"] += (blocked && y) as i64;
            t["fp"] += (blocked && !y) as i64;
            t["fn"] += (!blocked && y) as i64;
            let decision = res["decision"].as_str().unwrap_or("");
            if band != "safe" && (blocked || decision.starts_with("Hold") || rand::Rng::gen::<f64>(&mut rng) < 0.15) {
                let reasons: Vec<Value> = res["reasons"].as_array().map(|a| a.iter().take(3).cloned().collect()).unwrap_or_default();
                s.demo.feed.insert(0, json!({"ts": r.ts, "from": format!("{}·{}", r.fb, r.fa), "to": format!("{}·{}", r.tb, r.ta),
                    "amount": py_round(p.amount_paid, 2), "currency": r.cur_paid, "format": r.fmt, "risk": res["risk"], "decision": res["decision"],
                    "reasons": reasons, "case_id": res["case_id"], "truth": y}));
            }
        }
        s.demo.feed.truncate(60);
        lat.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let e = s.engine.as_ref().unwrap();
        ok(json!({"scored": batch.len(), "position": s.demo.ptr as i64 - s.demo.warm as i64, "total": rows.len() as i64 - s.demo.warm as i64,
                  "done": s.demo.ptr >= rows.len(), "exits": exits, "bands": bands, "truth": s.demo.truth,
                  "latency_p50_ms": if lat.is_empty() { Value::Null } else { jf(lat[lat.len() / 2]) }, "decisions": e.decisions,
                  "feed": s.demo.feed.iter().take(25).cloned().collect::<Vec<_>>(), "cases": e.cases(8), "open_cases": e.case_alerts.len(),
                  "sim_time": batch.last().map(|r| json!(r.ts)).unwrap_or(Value::Null)}))
    }).await
}

pub fn description() -> String {
    format!("{TAGLINE}. Live path: L1 XGBoost gatekeeper, L2 XGBoost + LightGBM average, L4 ring linker; L0 LightGBM scores account applications. \
             Trained and evaluated on synthetic datasets (IBM AMLworld, Feedzai BAF).")
}
