use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use iloader_core::storage::{derive_key, write_private_file};
use rand::RngCore;

/// Server configuration, read from environment variables (see README-SERVER.md).
#[derive(Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub static_dir: PathBuf,
    pub token: String,
    pub credential_key: Option<[u8; 32]>,
    pub max_upload_bytes: u64,
    pub cookie_secure: bool,
    /// Extra origins allowed besides the request's own host (e.g. behind a proxy).
    pub allowed_origins: Vec<String>,
    /// Level of logs streamed to the web UI (1=trace .. 5=error).
    pub ui_log_level: tracing::Level,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn default_data_dir() -> PathBuf {
    if let Some(d) = env("XDG_DATA_HOME") {
        return PathBuf::from(d).join("iloader-server");
    }
    if let Some(h) = env("HOME") {
        return PathBuf::from(h).join(".local/share/iloader-server");
    }
    PathBuf::from("./iloader-data")
}

fn parse_level(s: &str) -> Option<tracing::Level> {
    s.trim().parse().ok()
}

impl ServerConfig {
    pub fn from_env() -> Result<Self, String> {
        let bind = env("ILOADER_BIND")
            .unwrap_or_else(|| "127.0.0.1:8080".into())
            .parse()
            .map_err(|e| format!("ILOADER_BIND: {e}"))?;
        let data_dir = env("ILOADER_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(default_data_dir);
        let temp_dir = env("ILOADER_TEMP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("iloader-server"));
        let static_dir = env("ILOADER_STATIC_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("dist"));

        iloader_core::storage::create_private_dir(&data_dir)
            .map_err(|e| format!("data dir {}: {e}", data_dir.display()))?;

        let token = load_token(&data_dir)?;

        let credential_key = if let Some(path) = env("ILOADER_SECRET_KEY_FILE") {
            let material = std::fs::read(&path)
                .map_err(|e| format!("ILOADER_SECRET_KEY_FILE ({path}): {e}"))?;
            let material = trim_ascii(&material);
            if material.len() < 16 {
                return Err("ILOADER_SECRET_KEY_FILE must contain at least 16 bytes".into());
            }
            Some(derive_key(material))
        } else {
            None
        };

        let max_upload_mb: u64 = env("ILOADER_MAX_UPLOAD_MB")
            .map(|v| v.parse().map_err(|e| format!("ILOADER_MAX_UPLOAD_MB: {e}")))
            .transpose()?
            .unwrap_or(2048);

        let cookie_secure = matches!(
            env("ILOADER_COOKIE_SECURE").as_deref(),
            Some("1" | "true" | "yes")
        );

        let allowed_origins = env("ILOADER_ALLOWED_ORIGINS")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().trim_end_matches('/').to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let ui_log_level = env("ILOADER_UI_LOG_LEVEL")
            .map(|v| parse_level(&v).ok_or(format!("ILOADER_UI_LOG_LEVEL: invalid level {v}")))
            .transpose()?
            .unwrap_or(tracing::Level::DEBUG);

        Ok(Self {
            bind,
            data_dir,
            temp_dir,
            static_dir,
            token,
            credential_key,
            max_upload_bytes: max_upload_mb * 1024 * 1024,
            cookie_secure,
            allowed_origins,
            ui_log_level,
        })
    }
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(start, |e| e + 1);
    &b[start..end]
}

pub fn token_file(data_dir: &Path) -> PathBuf {
    data_dir.join("access_token")
}

/// `ILOADER_TOKEN`, else `ILOADER_TOKEN_FILE`, else `<data>/access_token`
/// (generated on first run).
fn load_token(data_dir: &Path) -> Result<String, String> {
    if let Some(t) = env("ILOADER_TOKEN") {
        return check_token(t.trim().to_string());
    }
    let path = env("ILOADER_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| token_file(data_dir));
    match std::fs::read_to_string(&path) {
        Ok(t) => check_token(t.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut bytes = [0u8; 24];
            rand::rng().fill_bytes(&mut bytes);
            let token = hex::encode(bytes);
            write_private_file(&path, format!("{token}\n").as_bytes())
                .map_err(|e| format!("writing {}: {e}", path.display()))?;
            eprintln!(
                "iloader-server: generated a new access token in {} (mode 0600)",
                path.display()
            );
            Ok(token)
        }
        Err(e) => Err(format!("reading token file {}: {e}", path.display())),
    }
}

fn check_token(t: String) -> Result<String, String> {
    if t.len() < 12 {
        return Err("the access token must have at least 12 characters".into());
    }
    Ok(t)
}
