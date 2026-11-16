//! `POST /api/invoke/:command` mirrors Tauri's `invoke`, plus the endpoints
//! that replace desktop-only features (file dialog → upload, save dialog →
//! download, Tauri events → `/api/emit`).

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path as UrlPath, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use iloader_core::{
    AppError, account, device,
    device::DeviceInfo,
    pairing, sideload,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use crate::{AppState, events::PROMPTS};

pub fn json_error(status: StatusCode, kind: &str, message: &str) -> Response {
    (status, Json(json!({ "type": kind, "message": message }))).into_response()
}

fn app_error(e: AppError) -> Response {
    let status = match e {
        AppError::NotLoggedIn | AppError::NoDeviceSelected | AppError::Canceled(_) => {
            StatusCode::CONFLICT
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(e)).into_response()
}

fn busy() -> Response {
    json_error(
        StatusCode::CONFLICT,
        "busy",
        "Another operation is already running on the server. Wait for it to finish.",
    )
}

fn ok<T: serde::Serialize>(r: Result<T, AppError>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => app_error(e),
    }
}

/// Upload ids are 32 lowercase hex chars (a UUID without dashes). Anything
/// else — paths, `../`, absolute paths — is rejected.
pub fn is_valid_upload_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn upload_path(uploads_dir: &Path, id: &str) -> Option<PathBuf> {
    is_valid_upload_id(id).then(|| uploads_dir.join(format!("{id}.ipa")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginNewArgs {
    email: String,
    password: String,
    anisette_server: String,
    #[serde(default)]
    save_credentials: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginStoredArgs {
    email: String,
    anisette_server: String,
}

#[derive(Deserialize)]
struct EmailArgs {
    email: String,
}

#[derive(Deserialize)]
struct SelectDeviceArgs {
    #[serde(default)]
    device: Option<DeviceInfo>,
}

#[derive(Deserialize)]
struct DeviceArgs {
    device: DeviceInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SideloadArgs {
    /// The id returned by `POST /api/upload` (named like the Tauri argument).
    app_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidestoreArgs {
    nightly: bool,
    live_container: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SerialArgs {
    serial_number: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppIdArgs {
    app_id_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlacePairingArgs {
    bundle_id: String,
    path: String,
}

#[derive(Deserialize)]
struct ForceArgs {
    force: bool,
}

fn args<T: DeserializeOwned>(v: Value) -> Result<T, Response> {
    serde_json::from_value(v).map_err(|e| {
        json_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            &format!("Invalid arguments: {e}"),
        )
    })
}

macro_rules! parse {
    ($v:expr) => {
        match args($v) {
            Ok(a) => a,
            Err(r) => return r,
        }
    };
}

/// Commands that use the Apple account / the device for a while. Only one of
/// them may run at a time (the Sideloader is taken out of its mutex while in
/// use, so a concurrent call would otherwise fail with a confusing NotLoggedIn).
const EXCLUSIVE: &[&str] = &[
    "login_new",
    "login_stored",
    "sideload_operation",
    "install_sidestore_operation",
    "get_certificates",
    "revoke_certificate",
    "list_app_ids",
    "delete_app_id",
];

pub const COMMANDS: &[&str] = &[
    "login_new",
    "login_stored",
    "delete_account",
    "logged_in_as",
    "invalidate_account",
    "reset_anisette_state",
    "list_devices",
    "set_selected_device",
    "cancel_pairing",
    "sideload_operation",
    "install_sidestore_operation",
    "get_certificates",
    "revoke_certificate",
    "list_app_ids",
    "delete_app_id",
    "installed_pairing_apps",
    "place_pairing_cmd",
    "export_pairing_cmd",
    "delete_stored_rppairing",
    "has_stored_rppairing",
    "keyring_available",
    "force_disable_keyring",
    // server-only
    "saved_accounts",
];

pub async fn invoke(
    State(state): State<Arc<AppState>>,
    UrlPath(command): UrlPath<String>,
    body: Bytes,
) -> Response {
    if !COMMANDS.contains(&command.as_str()) {
        return json_error(
            StatusCode::NOT_FOUND,
            "unknown_command",
            &format!("Unknown command {command}"),
        );
    }
    let value: Value = if body.iter().all(|b| b.is_ascii_whitespace()) {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(Value::Null) => json!({}),
            Ok(v) => v,
            Err(e) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    &format!("Invalid JSON: {e}"),
                );
            }
        }
    };

    let _guard = if EXCLUSIVE.contains(&command.as_str()) {
        match state.op_lock.clone().try_lock_owned() {
            Ok(g) => Some(g),
            Err(_) => return busy(),
        }
    } else {
        None
    };

    // Run detached so a closed tab or dropped connection does not abort an
    // install half-way; the lock guard travels with the task.
    let task_state = state.clone();
    let handle = tokio::spawn(async move {
        let _guard = _guard;
        dispatch(&task_state, &command, value).await
    });
    match handle.await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("command task failed: {e}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "misc", "Command task failed")
        }
    }
}

async fn dispatch(state: &AppState, command: &str, v: Value) -> Response {
    let ctx = &state.core;
    match command {
        "login_new" => {
            let a: LoginNewArgs = parse!(v);
            ok(account::login_new(ctx, a.email, a.password, a.anisette_server, a.save_credentials).await)
        }
        "login_stored" => {
            let a: LoginStoredArgs = parse!(v);
            ok(account::login_stored(ctx, a.email, a.anisette_server).await)
        }
        "delete_account" => {
            let a: EmailArgs = parse!(v);
            ok(account::delete_account(ctx, a.email))
        }
        "saved_accounts" => ok(account::saved_accounts(ctx)),
        "logged_in_as" => Json(account::logged_in_as(ctx)).into_response(),
        "invalidate_account" => {
            account::invalidate_account(ctx);
            Json(()).into_response()
        }
        "reset_anisette_state" => ok(account::reset_anisette_state(ctx)),
        "list_devices" => ok(device::list_devices().await),
        "set_selected_device" => {
            let a: SelectDeviceArgs = parse!(v);
            ok(device::set_selected_device(ctx, a.device).await)
        }
        "cancel_pairing" => ok(device::cancel_pairing(ctx)),
        "sideload_operation" => {
            let a: SideloadArgs = parse!(v);
            let Some(path) = upload_path(&state.uploads_dir, &a.app_path) else {
                return json_error(StatusCode::BAD_REQUEST, "bad_request", "Invalid upload id");
            };
            if !path.is_file() {
                return json_error(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "Uploaded file not found (it may have expired); upload it again",
                );
            }
            let r = sideload::sideload_operation(ctx, &path).await;
            let _ = tokio::fs::remove_file(&path).await;
            ok(r)
        }
        "install_sidestore_operation" => {
            let a: SidestoreArgs = parse!(v);
            ok(sideload::install_sidestore_operation(ctx, a.nightly, a.live_container).await)
        }
        "get_certificates" => ok(account::get_certificates(ctx).await),
        "revoke_certificate" => {
            let a: SerialArgs = parse!(v);
            ok(account::revoke_certificate(ctx, a.serial_number).await)
        }
        "list_app_ids" => ok(account::list_app_ids(ctx).await),
        "delete_app_id" => {
            let a: AppIdArgs = parse!(v);
            ok(account::delete_app_id(ctx, a.app_id_id).await)
        }
        "installed_pairing_apps" => ok(pairing::installed_pairing_apps(ctx).await),
        "place_pairing_cmd" => {
            let a: PlacePairingArgs = parse!(v);
            ok(pairing::place_pairing_cmd(ctx, a.bundle_id, a.path).await)
        }
        "export_pairing_cmd" => export_pairing_response(state),
        "delete_stored_rppairing" => ok(pairing::delete_stored_rppairing(ctx)),
        "has_stored_rppairing" => {
            let a: DeviceArgs = parse!(v);
            ok(pairing::has_stored_rppairing(ctx, a.device))
        }
        "keyring_available" => Json(ctx.credential_saving_available()).into_response(),
        "force_disable_keyring" => {
            let a: ForceArgs = parse!(v);
            ctx.set_credential_saving_disabled(a.force);
            Json(()).into_response()
        }
        _ => json_error(StatusCode::NOT_FOUND, "unknown_command", "Unknown command"),
    }
}

fn export_pairing_response(state: &AppState) -> Response {
    match pairing::export_pairing(&state.core) {
        Ok(bytes) => {
            let mut r = Response::new(Body::from(bytes));
            let h = r.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-plist"));
            h.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"pairingFile.plist\""),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Err(e) => app_error(e),
    }
}

/// `GET /api/pairing/export`: the pairing file of the selected device as a download.
pub async fn export_pairing(State(state): State<Arc<AppState>>) -> Response {
    export_pairing_response(&state)
}

/// `POST /api/emit/:event`: answers to prompts (`2fa-recieved`, `max-certs-response`).
pub async fn emit(
    State(state): State<Arc<AppState>>,
    UrlPath(event): UrlPath<String>,
    body: Bytes,
) -> Response {
    if !PROMPTS.iter().any(|(_, answer)| *answer == event) {
        return json_error(StatusCode::NOT_FOUND, "unknown_event", "Unknown event");
    }
    let payload: Value = if body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return json_error(StatusCode::BAD_REQUEST, "bad_request", &format!("Invalid JSON: {e}"));
            }
        }
    };
    let delivered = state.core.prompts.deliver(&event, payload);
    Json(json!({ "delivered": delivered })).into_response()
}

/// `POST /api/upload`: raw IPA bytes in the body. Returns `{ "id": ... }`, to
/// be passed as `appPath` to `sideload_operation`.
pub async fn upload(State(state): State<Arc<AppState>>, body: Body) -> Response {
    cleanup_uploads(&state.uploads_dir, Duration::from_secs(2 * 3600)).await;

    let id = uuid::Uuid::new_v4().simple().to_string();
    let path = state.uploads_dir.join(format!("{id}.ipa"));
    let res = async {
        let mut file = tokio::fs::File::create(&path).await?;
        let mut stream = body.into_data_stream();
        let mut total: u64 = 0;
        let mut first = true;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(std::io::Error::other)?;
            if first && !chunk.is_empty() {
                first = false;
                // IPA files are zip archives
                if !chunk.starts_with(b"PK") {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "not an IPA (zip) file",
                    ));
                }
            }
            total += chunk.len() as u64;
            if total > state.config.max_upload_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "file too large",
                ));
            }
            file.write_all(&chunk).await?;
        }
        if total == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "empty upload"));
        }
        file.flush().await?;
        Ok::<u64, std::io::Error>(total)
    }
    .await;

    match res {
        Ok(size) => {
            tracing::info!(target: "upload", "Received IPA upload {id} ({size} bytes)");
            Json(json!({ "id": id, "size": size })).into_response()
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            let status = match e.kind() {
                std::io::ErrorKind::FileTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
                std::io::ErrorKind::InvalidData => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            json_error(status, "upload", &format!("Upload failed: {e}"))
        }
    }
}

/// Removes uploads older than `max_age`.
pub async fn cleanup_uploads(dir: &Path, max_age: Duration) {
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let now = SystemTime::now();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let old = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= max_age);
        if old {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

pub async fn version() -> impl IntoResponse {
    Json(json!({ "version": env!("CARGO_PKG_VERSION"), "headless": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_ids() {
        let dir = Path::new("/srv/uploads");
        assert!(upload_path(dir, "0123456789abcdef0123456789abcdef").is_some());
        for bad in [
            "../../etc/passwd",
            "/etc/passwd",
            "0123456789abcdef0123456789abcde",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcde/",
            "..%2f..%2fetc%2fpasswd",
            "",
        ] {
            assert!(upload_path(dir, bad).is_none(), "{bad:?}");
        }
    }
}
