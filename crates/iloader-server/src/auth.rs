//! Access control: one shared access token, exchanged for an HttpOnly session
//! cookie (or sent directly as `Authorization: Bearer <token>`), plus a simple
//! per-IP rate limit on failed logins and an `Origin` check against CSRF and
//! cross-site WebSocket hijacking.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rand::RngCore;
use serde::Deserialize;
use sha2_shim::digest;
use subtle::ConstantTimeEq;

use crate::{AppState, api::json_error};

pub const COOKIE_NAME: &str = "iloader_session";
const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
const FAIL_WINDOW: Duration = Duration::from_secs(5 * 60);
const MAX_FAILS: usize = 5;

/// Tiny wrapper so we don't need a direct sha2 dependency: hashing both sides
/// to a fixed length before the constant-time comparison avoids leaking the
/// token length.
mod sha2_shim {
    pub fn digest(data: &[u8]) -> [u8; 32] {
        iloader_core::storage::derive_key(data)
    }
}

pub struct Auth {
    token_digest: [u8; 32],
    sessions: Mutex<HashMap<String, Instant>>,
    failures: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl Auth {
    pub fn new(token: &str) -> Self {
        Self {
            token_digest: digest(token.as_bytes()),
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        }
    }

    pub fn token_matches(&self, candidate: &str) -> bool {
        bool::from(digest(candidate.as_bytes()).ct_eq(&self.token_digest))
    }

    pub fn create_session(&self) -> String {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let id = hex::encode(bytes);
        let mut s = self.sessions.lock().unwrap();
        let now = Instant::now();
        s.retain(|_, exp| *exp > now);
        s.insert(id.clone(), now + SESSION_TTL);
        id
    }

    pub fn session_valid(&self, id: &str) -> bool {
        let s = self.sessions.lock().unwrap();
        // constant-time over the stored ids is overkill: ids are 256-bit random
        s.get(id).is_some_and(|exp| *exp > Instant::now())
    }

    pub fn drop_session(&self, id: &str) {
        self.sessions.lock().unwrap().remove(id);
    }

    /// `Err(retry_after)` when the IP is currently locked out.
    pub fn check_rate_limit(&self, ip: IpAddr) -> Result<(), Duration> {
        let mut f = self.failures.lock().unwrap();
        let now = Instant::now();
        let entry = f.entry(ip).or_default();
        entry.retain(|t| now.duration_since(*t) < FAIL_WINDOW);
        if entry.len() >= MAX_FAILS {
            let oldest = entry[0];
            return Err(FAIL_WINDOW.saturating_sub(now.duration_since(oldest)));
        }
        Ok(())
    }

    pub fn record_failure(&self, ip: IpAddr) {
        self.failures
            .lock()
            .unwrap()
            .entry(ip)
            .or_default()
            .push(Instant::now());
    }

    pub fn clear_failures(&self, ip: IpAddr) {
        self.failures.lock().unwrap().remove(&ip);
    }

    /// Whether the request carries a valid session cookie or bearer token.
    pub fn is_authenticated(&self, headers: &HeaderMap) -> bool {
        if let Some(id) = session_cookie(headers)
            && self.session_valid(&id)
        {
            return true;
        }
        if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
            && let Some(t) = v.strip_prefix("Bearer ")
        {
            return self.token_matches(t.trim());
        }
        false
    }
}

pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| cookie::Cookie::split_parse(v.to_string()))
        .filter_map(Result::ok)
        .find(|c| c.name() == COOKIE_NAME)
        .map(|c| c.value().to_string())
}

fn client_ip(req: &Request) -> IpAddr {
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::from([0, 0, 0, 0]))
}

/// Rejects cross-origin requests. Browsers always send `Origin` on WebSocket
/// handshakes and on cross-origin/unsafe fetches; requests without it (curl,
/// same-origin GETs) are let through and still need the token.
pub fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin = origin.trim_end_matches('/');
    if allowed.iter().any(|a| a == origin) {
        return true;
    }
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok());
    let Some(host) = host else { return false };
    let origin_host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    origin_host == Some(host)
}

/// Middleware for every `/api` route except login.
pub async fn require_auth(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let is_ws = req
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let unsafe_method = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if (is_ws || unsafe_method) && !origin_allowed(req.headers(), &state.config.allowed_origins) {
        return json_error(StatusCode::FORBIDDEN, "forbidden", "Origin not allowed");
    }
    if !state.auth.is_authenticated(req.headers()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", "Authentication required");
    }
    next.run(req).await
}

#[derive(Deserialize)]
pub struct LoginBody {
    token: String,
}

pub async fn login(State(state): State<Arc<AppState>>, req: Request) -> Response {
    if !origin_allowed(req.headers(), &state.config.allowed_origins) {
        return json_error(StatusCode::FORBIDDEN, "forbidden", "Origin not allowed");
    }
    let ip = client_ip(&req);
    if let Err(retry) = state.auth.check_rate_limit(ip) {
        let mut r = json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many failed attempts, try again later",
        );
        if let Ok(v) = HeaderValue::from_str(&retry.as_secs().max(1).to_string()) {
            r.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return r;
    }
    let bytes = match axum::body::to_bytes(req.into_body(), 4096).await {
        Ok(b) => b,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "bad_request", "Invalid body"),
    };
    let Ok(body) = serde_json::from_slice::<LoginBody>(&bytes) else {
        return json_error(StatusCode::BAD_REQUEST, "bad_request", "Expected {\"token\": ...}");
    };
    if !state.auth.token_matches(body.token.trim()) {
        state.auth.record_failure(ip);
        tracing::warn!(target: "auth", "Failed server login from {ip}");
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", "Invalid access token");
    }
    state.auth.clear_failures(ip);
    let id = state.auth.create_session();
    let mut c = cookie::Cookie::build((COOKIE_NAME, id))
        .path("/")
        .http_only(true)
        .same_site(cookie::SameSite::Strict)
        .max_age(cookie::time::Duration::seconds(SESSION_TTL.as_secs() as i64));
    if state.config.cookie_secure {
        c = c.secure(true);
    }
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = HeaderValue::from_str(&c.build().to_string()) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(id) = session_cookie(&headers) {
        state.auth.drop_session(&id);
    }
    let c = cookie::Cookie::build((COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .same_site(cookie::SameSite::Strict)
        .max_age(cookie::time::Duration::ZERO);
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = HeaderValue::from_str(&c.build().to_string()) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

pub async fn session() -> impl IntoResponse {
    Json(serde_json::json!({ "authenticated": true }))
}
