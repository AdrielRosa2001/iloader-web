//! Server tests that don't need an iPhone or an Apple account.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use iloader_core::EventSink;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;

const TOKEN: &str = "test-token-0123456789";

struct Env {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    router: Router,
}

fn env_with(max_upload_bytes: u64) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let config = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.path().join("data"),
        temp_dir: dir.path().join("tmp"),
        static_dir: dir.path().join("dist"),
        token: TOKEN.into(),
        credential_key: None,
        max_upload_bytes,
        cookie_secure: false,
        allowed_origins: vec![],
        ui_log_level: tracing::Level::DEBUG,
    };
    std::fs::create_dir_all(&config.static_dir).unwrap();
    std::fs::write(config.static_dir.join("index.html"), "<html>ui</html>").unwrap();
    let state = Arc::new(AppState::new(config, Arc::new(BroadcastSink::new())).unwrap());
    let router = build_router(state.clone());
    Env {
        _dir: dir,
        state,
        router,
    }
}

fn env() -> Env {
    env_with(10 * 1024 * 1024)
}

fn req(method: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "srv.local:8080")
}

fn authed(method: &str, uri: &str) -> axum::http::request::Builder {
    req(method, uri).header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
}

async fn send(router: &Router, r: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let resp = router.clone().oneshot(r).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, body)
}

fn json_of(b: &[u8]) -> Value {
    serde_json::from_slice(b).unwrap()
}

#[tokio::test]
async fn every_api_route_requires_auth() {
    let e = env();
    let cases = [
        ("GET", "/api/auth/session"),
        ("POST", "/api/invoke/list_devices"),
        ("POST", "/api/invoke/logged_in_as"),
        ("POST", "/api/emit/2fa-recieved"),
        ("POST", "/api/upload"),
        ("GET", "/api/pairing/export"),
        ("GET", "/api/version"),
        ("GET", "/api/events"),
    ];
    for (m, u) in cases {
        let (s, _, _) = send(&e.router, req(m, u).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {u}");
        // wrong bearer token
        let r = req(m, u)
            .header(header::AUTHORIZATION, "Bearer nope-nope-nope")
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&e.router, r).await.0, StatusCode::UNAUTHORIZED, "{m} {u}");
        // forged cookie
        let r = req(m, u)
            .header(header::COOKIE, format!("{}=deadbeef", auth::COOKIE_NAME))
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&e.router, r).await.0, StatusCode::UNAUTHORIZED, "{m} {u}");
    }
    // websocket handshake without auth
    let r = req("GET", "/api/events")
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&e.router, r).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn static_ui_is_public() {
    let e = env();
    let (s, h, b) = send(&e.router, req("GET", "/").body(Body::empty()).unwrap()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, b"<html>ui</html>");
    assert!(h.contains_key("content-security-policy"));
    // SPA fallback
    let (s, _, _) = send(&e.router, req("GET", "/some/route").body(Body::empty()).unwrap()).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn login_cookie_session_and_logout() {
    let e = env();
    let login = |token: &str| {
        req("POST", "/api/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "token": token }).to_string()))
            .unwrap()
    };
    let (s, _, _) = send(&e.router, login("wrong-token-123")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, h, _) = send(&e.router, login(TOKEN)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let set_cookie = h.get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    let cookie_pair = set_cookie.split(';').next().unwrap().to_string();

    let with_cookie = |m: &str, u: &str| {
        req(m, u)
            .header(header::COOKIE, &cookie_pair)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(send(&e.router, with_cookie("GET", "/api/auth/session")).await.0, StatusCode::OK);
    let (s, _, b) = send(&e.router, with_cookie("POST", "/api/invoke/logged_in_as")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(json_of(&b), Value::Null);

    assert_eq!(
        send(&e.router, with_cookie("POST", "/api/auth/logout")).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&e.router, with_cookie("GET", "/api/auth/session")).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn login_is_rate_limited() {
    let e = env();
    let login = |token: &str| {
        req("POST", "/api/auth/login")
            .body(Body::from(json!({ "token": token }).to_string()))
            .unwrap()
    };
    for _ in 0..5 {
        assert_eq!(send(&e.router, login("bad-token-xxxx")).await.0, StatusCode::UNAUTHORIZED);
    }
    let (s, h, _) = send(&e.router, login(TOKEN)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "even the right token is refused while locked out");
    assert!(h.contains_key(header::RETRY_AFTER));
}

#[tokio::test]
async fn cross_origin_requests_are_rejected() {
    let e = env();
    let r = authed("POST", "/api/invoke/logged_in_as")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&e.router, r).await.0, StatusCode::FORBIDDEN);
    let r = authed("POST", "/api/invoke/logged_in_as")
        .header(header::ORIGIN, "http://srv.local:8080")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&e.router, r).await.0, StatusCode::OK);
    let r = req("POST", "/api/auth/login")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::from(json!({ "token": TOKEN }).to_string()))
        .unwrap();
    assert_eq!(send(&e.router, r).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn simple_commands() {
    let e = env();
    let call = |cmd: &str, body: Value| {
        authed("POST", &format!("/api/invoke/{cmd}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (s, _, b) = send(&e.router, call("keyring_available", json!({}))).await;
    assert_eq!((s, json_of(&b)), (StatusCode::OK, json!(false)));
    let (s, _, b) = send(&e.router, call("saved_accounts", json!(null))).await;
    assert_eq!((s, json_of(&b)), (StatusCode::OK, json!([])));
    let (s, _, b) = send(&e.router, call("reset_anisette_state", json!({}))).await;
    assert_eq!((s, json_of(&b)), (StatusCode::OK, json!(false)));
    let (s, _, _) = send(&e.router, call("force_disable_keyring", json!({ "force": true }))).await;
    assert_eq!(s, StatusCode::OK);
    // set_selected_device without arguments (as Settings.tsx does) clears the selection
    let (s, _, _) = send(&e.router, call("set_selected_device", json!({}))).await;
    assert_eq!(s, StatusCode::OK);

    let (s, _, b) = send(&e.router, call("get_certificates", json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(json_of(&b)["type"], "not_logged_in");
    let (s, _, b) = send(&e.router, call("installed_pairing_apps", json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(json_of(&b)["type"], "no_device_selected");
    let (s, _, b) = send(&e.router, authed("GET", "/api/pairing/export").body(Body::empty()).unwrap()).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(json_of(&b)["type"], "no_device_selected");

    let (s, _, _) = send(&e.router, call("does_not_exist", json!({}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = send(&e.router, call("revoke_certificate", json!({ "wrong": 1 }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, b) = send(&e.router, call("login_new", json!({
        "email": "a@b.c", "password": "x", "anisetteServer": "ani.example", "saveCredentials": true
    }))).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(json_of(&b)["type"], "keyring_with_message");
}

#[tokio::test]
async fn sideload_only_accepts_upload_ids() {
    let e = env();
    for bad in ["../../etc/passwd", "/etc/passwd", "uploads/../x.ipa", "C:\\x.ipa"] {
        let r = authed("POST", "/api/invoke/sideload_operation")
            .body(Body::from(json!({ "appPath": bad }).to_string()))
            .unwrap();
        let (s, _, b) = send(&e.router, r).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(json_of(&b)["message"], "Invalid upload id");
    }
    let r = authed("POST", "/api/invoke/sideload_operation")
        .body(Body::from(json!({ "appPath": "0123456789abcdef0123456789abcdef" }).to_string()))
        .unwrap();
    assert_eq!(send(&e.router, r).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn uploads() {
    let e = env_with(1024);
    let up = |body: Vec<u8>| authed("POST", "/api/upload").body(Body::from(body)).unwrap();

    let (s, _, _) = send(&e.router, up(b"not a zip".to_vec())).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = send(&e.router, up(vec![])).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let mut big = b"PK\x03\x04".to_vec();
    big.resize(2048, 0);
    let (s, _, _) = send(&e.router, up(big)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);

    let (s, _, b) = send(&e.router, up(b"PK\x03\x04hello".to_vec())).await;
    assert_eq!(s, StatusCode::OK);
    let id = json_of(&b)["id"].as_str().unwrap().to_string();
    assert!(api::is_valid_upload_id(&id));
    let path = e.state.uploads_dir.join(format!("{id}.ipa"));
    assert_eq!(std::fs::read(&path).unwrap(), b"PK\x03\x04hello");
    // only the valid upload is left on disk
    assert_eq!(std::fs::read_dir(&e.state.uploads_dir).unwrap().count(), 1);

    // the sideload consumes (deletes) the upload even when it fails (no device)
    let r = authed("POST", "/api/invoke/sideload_operation")
        .body(Body::from(json!({ "appPath": id }).to_string()))
        .unwrap();
    let (s, _, b) = send(&e.router, r).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(json_of(&b)["type"], "no_device_selected");
    assert!(!path.exists());
}

#[tokio::test]
async fn exclusive_commands_return_busy() {
    let e = env();
    let _held = e.state.op_lock.clone().lock_owned().await;
    for cmd in ["list_app_ids", "login_stored", "install_sidestore_operation"] {
        let (s, _, b) = send(
            &e.router,
            authed("POST", &format!("/api/invoke/{cmd}")).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "{cmd}");
        assert_eq!(json_of(&b)["type"], "busy");
    }
    // non-exclusive commands still work during an operation
    let (s, _, _) = send(
        &e.router,
        authed("POST", "/api/invoke/logged_in_as").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn emit_answers_pending_prompt() {
    let e = env();
    let answer = e.state.core.prompts.register("2fa-recieved", Duration::from_secs(5));
    let (s, _, b) = send(
        &e.router,
        authed("POST", "/api/emit/2fa-recieved").body(Body::from("\"654321\"")).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(json_of(&b), json!({ "delivered": true }));
    assert_eq!(answer.await.unwrap(), json!("654321"));

    let (s, _, b) = send(
        &e.router,
        authed("POST", "/api/emit/max-certs-response").body(Body::from("null")).unwrap(),
    )
    .await;
    assert_eq!((s, json_of(&b)), (StatusCode::OK, json!({ "delivered": false })));
    let (s, _, _) = send(
        &e.router,
        authed("POST", "/api/emit/operation_sideload").body(Body::from("{}")).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// Real TCP server + WebSocket client: events reach authenticated clients, and
/// prompts that are still open are replayed on connect.
#[tokio::test]
async fn websocket_receives_events() {
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

    let e = env();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = e.router.clone();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    });

    // without credentials the handshake fails with 401
    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/api/events")).await.unwrap_err();
    assert!(err.to_string().contains("401"), "{err}");

    // a prompt opened before the client connects
    let _answer = e.state.core.prompts.register("2fa-recieved", Duration::from_secs(30));
    e.state.sink.emit("2fa-required", json!({ "kind": "trusted-device" }));

    let mut request = format!("ws://{addr}/api/events").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();

    macro_rules! next {
        () => {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Value>(&t).unwrap(),
                other => panic!("unexpected {other:?}"),
            }
        };
    }
    let replay = next!();
    assert_eq!(replay["event"], "2fa-required");

    // give the server a moment to subscribe, then emit through the core sink
    tokio::time::sleep(Duration::from_millis(50)).await;
    e.state.core.events.emit(
        "operation_sideload",
        json!({ "updateType": "started", "stepId": "install", "extraDetails": null }),
    );
    let ev = next!();
    assert_eq!(ev["event"], "operation_sideload");
    assert_eq!(ev["payload"]["stepId"], "install");
}
