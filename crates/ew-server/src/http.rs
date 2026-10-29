//! Teacher dashboard: static files, JSON API, WebSocket live feed.
//!
//! Auth: Argon2id passwords, random 256-bit session cookie (HttpOnly,
//! SameSite=Strict, Secure under TLS). State-changing requests also require an
//! `X-EW: 1` header, which a cross-site form cannot send.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use rand::RngCore;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

use crate::hub::Hub;
use crate::recorder;

const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
const COOKIE: &str = "ew_session";

pub struct AppState {
    pub hub: Arc<Hub>,
    sessions: Mutex<HashMap<String, Session>>,
    fails: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    secure_cookie: bool,
}

#[derive(Clone)]
struct Session {
    user: String,
    admin: bool,
    expires: Instant,
}

pub fn router(hub: Arc<Hub>, secure_cookie: bool) -> Router {
    let state = Arc::new(AppState {
        hub,
        sessions: Mutex::new(HashMap::new()),
        fails: Mutex::new(HashMap::new()),
        secure_cookie,
    });
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/app.css", get(app_css))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/exams", post(open_exam))
        .route("/api/exams/history", get(exam_history))
        .route("/api/exams/:id/close", post(close_exam))
        .route("/api/exams/:id/recordings", get(recordings))
        .route("/api/exams/:id/students/:sid/segments", get(segments))
        .route("/api/segments/:id", get(segment_data))
        .route("/api/pcs/:mid/release", post(release_pc))
        .route("/api/audit", get(audit))
        .route("/ws", get(ws))
        .with_state(state)
}

// ------------------------------------------------------------------ static

const INDEX: &str = include_str!("../../../dashboard/index.html");
const APP_JS: &str = include_str!("../../../dashboard/app.js");
const APP_CSS: &str = include_str!("../../../dashboard/app.css");

fn asset(ct: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, ct),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'",
            ),
        ],
        body,
    )
        .into_response()
}

async fn index() -> Response {
    asset("text/html; charset=utf-8", INDEX)
}
async fn app_js() -> Response {
    asset("text/javascript; charset=utf-8", APP_JS)
}
async fn app_css() -> Response {
    asset("text/css; charset=utf-8", APP_CSS)
}

// -------------------------------------------------------------------- auth

pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default().hash_password(pw.as_bytes(), &salt).map_err(|e| anyhow::anyhow!("{e}"))?.to_string())
}

fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash).map_or(false, |h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

struct Teacher {
    user: String,
    #[allow(dead_code)]
    admin: bool,
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
}

#[async_trait]
impl FromRequestParts<Arc<AppState>> for Teacher {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, st: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let unauth = || (StatusCode::UNAUTHORIZED, Json(json!({"error": "Please sign in"}))).into_response();
        let token = cookie_token(&parts.headers).ok_or_else(unauth)?;
        let s = {
            let mut sessions = st.sessions.lock();
            match sessions.get_mut(&token) {
                Some(s) if s.expires > Instant::now() => {
                    s.expires = Instant::now() + SESSION_TTL; // sliding expiry
                    s.clone()
                }
                Some(_) => {
                    sessions.remove(&token);
                    return Err(unauth());
                }
                None => return Err(unauth()),
            }
        };
        if parts.method != axum::http::Method::GET && parts.headers.get("x-ew").map(|v| v.as_bytes()) != Some(b"1") {
            return Err((StatusCode::FORBIDDEN, Json(json!({"error": "Missing request header"}))).into_response());
        }
        Ok(Teacher { user: s.user, admin: s.admin })
    }
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

async fn login(State(st): State<Arc<AppState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(req): Json<LoginReq>) -> Response {
    if headers.get("x-ew").map(|v| v.as_bytes()) != Some(b"1") {
        return err(StatusCode::FORBIDDEN, "Missing request header");
    }
    let ip = addr.ip();
    // 5 failures → 60 s lockout per address.
    if let Some((n, until)) = st.fails.lock().get(&ip) {
        if *n >= 5 && *until > Instant::now() {
            return err(StatusCode::TOO_MANY_REQUESTS, "Too many attempts. Wait a minute and try again.");
        }
    }
    let row = st.hub.db.teacher_hash(req.username.trim()).ok().flatten();
    let pw = req.password.clone();
    let ok_row = tokio::task::spawn_blocking(move || match row {
        Some((u, h, admin)) if verify_password(&pw, &h) => Some((u, admin)),
        Some(_) => None,
        None => {
            // Spend the same time on unknown users so names can't be probed.
            let _ = verify_password(&pw, "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$Z2Fya2Fya2Fya2Fya2Fya2Fya2Fya2Fya2Fya2Fya2E");
            None
        }
    })
    .await
    .ok()
    .flatten();
    let Some((user, admin)) = ok_row else {
        let mut f = st.fails.lock();
        let e = f.entry(ip).or_insert((0, Instant::now()));
        e.0 += 1;
        e.1 = Instant::now() + Duration::from_secs(60);
        st.hub.db.audit(req.username.trim(), "login_failed", &ip.to_string());
        return err(StatusCode::UNAUTHORIZED, "Wrong username or password");
    };
    st.fails.lock().remove(&ip);
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let token = hex::encode(raw);
    st.sessions.lock().insert(token.clone(), Session { user: user.clone(), admin, expires: Instant::now() + SESSION_TTL });
    st.hub.db.audit(&user, "login", &ip.to_string());
    let cookie = format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{}",
        SESSION_TTL.as_secs(),
        if st.secure_cookie { "; Secure" } else { "" }
    );
    let mut resp = Json(json!({"user": user, "admin": admin})).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    resp
}

async fn logout(State(st): State<Arc<AppState>>, t: Teacher, headers: HeaderMap) -> Response {
    if let Some(tok) = cookie_token(&headers) {
        st.sessions.lock().remove(&tok);
    }
    st.hub.db.audit(&t.user, "logout", "");
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_static("ew_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"));
    resp
}

async fn me(t: Teacher) -> Response {
    Json(json!({"user": t.user})).into_response()
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, [(header::CACHE_CONTROL, "no-store")], Json(json!({"error": msg}))).into_response()
}

fn ok_json(v: serde_json::Value) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response()
}

// ------------------------------------------------------------------- exams

#[derive(Deserialize)]
struct OpenExamReq {
    name: String,
    labs: Vec<String>,
    /// Pasted or uploaded CSV/text; the first column of each row is the Student ID.
    #[serde(default)]
    roster: String,
}

pub fn parse_roster(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.split([',', ';', '\t']).next())
        .map(|c| ew_proto::normalize_student_id(c.trim_matches('"')))
        .filter(|id| ew_proto::student_id_format_ok(id))
        .filter(|id| !matches!(id.as_str(), "ID" | "STUDENTID" | "STUDENT_ID" | "STUDENT-ID"))
        .collect()
}

async fn open_exam(State(st): State<Arc<AppState>>, t: Teacher, Json(req): Json<OpenExamReq>) -> Response {
    let roster = parse_roster(&req.roster);
    if !req.roster.trim().is_empty() && roster.is_empty() {
        return err(StatusCode::BAD_REQUEST, "No valid Student IDs found in the roster");
    }
    match st.hub.open_exam(&t.user, &req.name, req.labs, roster) {
        Ok(e) => ok_json(json!(e)),
        Err(e) => err(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

async fn close_exam(State(st): State<Arc<AppState>>, t: Teacher, Path(id): Path<i64>) -> Response {
    match st.hub.close_exam(&t.user, id) {
        Ok(()) => ok_json(json!({"ok": true})),
        Err(e) => err(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

async fn release_pc(State(st): State<Arc<AppState>>, t: Teacher, Path(mid): Path<String>) -> Response {
    match st.hub.release_pc(&t.user, &mid) {
        Ok(()) => ok_json(json!({"ok": true})),
        Err(e) => err(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

async fn exam_history(State(st): State<Arc<AppState>>, _t: Teacher) -> Response {
    match st.hub.db.recent_exams(100) {
        Ok(v) => ok_json(json!({"exams": v, "retention_hours": st.hub.cfg.retention_hours})),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn recordings(State(st): State<Arc<AppState>>, _t: Teacher, Path(id): Path<i64>) -> Response {
    let exam = match st.hub.db.exam(id) {
        Ok(Some(e)) => e,
        _ => return err(StatusCode::NOT_FOUND, "Exam not found"),
    };
    match (st.hub.db.recording_summary(id), st.hub.db.claims(id)) {
        (Ok(rec), Ok(claims)) => ok_json(json!({"exam": exam, "students": rec, "claims": claims})),
        _ => err(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
    }
}

async fn segments(State(st): State<Arc<AppState>>, t: Teacher, Path((id, sid)): Path<(i64, String)>) -> Response {
    match st.hub.db.segments(id, &sid) {
        Ok(v) => {
            st.hub.db.audit(&t.user, "view_recording", &format!("exam {id} student {sid}"));
            ok_json(json!({"segments": v}))
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn segment_data(State(st): State<Arc<AppState>>, _t: Teacher, Path(id): Path<i64>) -> Response {
    let Ok(Some(seg)) = st.hub.db.segment(id) else { return err(StatusCode::NOT_FOUND, "Segment not found") };
    let Ok(Some(wrapped)) = st.hub.db.exam_key(seg.exam_id) else { return err(StatusCode::GONE, "Recording was deleted (retention)") };
    let master = *st.hub.master_key();
    let res = tokio::task::spawn_blocking(move || {
        let key = crate::crypto::unwrap_key(&master, &wrapped)?;
        recorder::read_segment(std::path::Path::new(&seg.path), &key)
    })
    .await;
    match res {
        Ok(Ok(body)) => (
            [(header::CONTENT_TYPE, "application/octet-stream"), (header::CACHE_CONTROL, "no-store")],
            body,
        )
            .into_response(),
        Ok(Err(e)) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("Cannot read segment: {e}")),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
    }
}

async fn audit(State(st): State<Arc<AppState>>, _t: Teacher) -> Response {
    match st.hub.db.audit_recent(200) {
        Ok(v) => ok_json(json!({"entries": v})),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// --------------------------------------------------------------- websocket

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum WsOp {
    Thumbs { lab: Option<String> },
    Live { pc: String, on: bool },
}

async fn ws(State(st): State<Arc<AppState>>, t: Teacher, upgrade: WebSocketUpgrade) -> Response {
    upgrade.max_message_size(64 * 1024).on_upgrade(move |sock| ws_session(st, t.user, sock))
}

async fn ws_session(st: Arc<AppState>, user: String, sock: WebSocket) {
    let (mut sink, mut stream) = sock.split();
    // ~a few seconds of thumbnails for a whole lab; beyond that frames are skipped to the next keyframe.
    let (tx, mut rx) = mpsc::channel::<Message>(512);
    let dash = st.hub.add_dash(&user, tx);
    let out = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Text(t) = msg {
            match serde_json::from_str::<WsOp>(&t) {
                Ok(WsOp::Thumbs { lab }) => st.hub.subscribe_thumbs(&dash, lab),
                Ok(WsOp::Live { pc, on }) => st.hub.set_live(&dash, &pc, on),
                Err(_) => {}
            }
        }
    }
    st.hub.remove_dash(&dash);
    out.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_parsing() {
        let r = parse_roster("Student ID,Name\n221-15-4021, Rahim\n\"221-15-4022\";Karim\n bad/id \n\n221 15 4023\tX");
        assert_eq!(r, vec!["221-15-4021", "221-15-4022", "221154023"]);
    }

    #[test]
    fn password_hash_verify() {
        let h = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn cookie_extraction() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; ew_session=abc; b=2"));
        assert_eq!(cookie_token(&h).as_deref(), Some("abc"));
    }
}
