//! Headless core of iloader: Apple ID login, device pairing and sideloading,
//! with no dependency on Tauri or any GUI toolkit.
//!
//! The front end (an HTTP server, a desktop shell, tests...) provides an
//! [`EventSink`] to receive progress/prompt events and answers prompts by
//! calling [`PromptHub::deliver`].

pub mod account;
pub mod device;
pub mod error;
pub mod operation;
pub mod pairing;
pub mod sideload;
pub mod storage;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::Value;
use tokio::sync::oneshot;

pub use error::AppError;

use crate::{
    device::{DeviceInfoMutex, PairingCancelToken},
    sideload::SideloaderMutex,
    storage::{CredentialStore, FileStorage},
};

/// Receives events going from the core to the user interface
/// (`operation_<id>`, `2fa-required`, `max-certs-reached`, `log-record`, ...).
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &str, payload: Value);
}

/// An [`EventSink`] that drops everything. Useful for tests.
pub struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _event: &str, _payload: Value) {}
}

/// Pending questions the core asked the user (2FA code, which certificates to
/// revoke). The UI answers by delivering a payload under the response event name.
#[derive(Default)]
pub struct PromptHub {
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    #[error("Timed out waiting for {0}")]
    Timeout(String),
    #[error("Prompt {0} was superseded or dropped")]
    Dropped(String),
}

impl PromptHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers interest in `response_event` and returns a future that resolves
    /// with the delivered payload. Register *before* emitting the question so a
    /// fast answer is never lost. A newer registration replaces an older one.
    pub fn register(
        &self,
        response_event: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<Value, PromptError>> + Send + 'static {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(response_event.to_string(), tx);
        let name = response_event.to_string();
        async move {
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(_)) => Err(PromptError::Dropped(name)),
                Err(_) => Err(PromptError::Timeout(name)),
            }
        }
    }

    /// Delivers an answer. Returns `false` when nobody was waiting for it.
    pub fn deliver(&self, response_event: &str, payload: Value) -> bool {
        let sender = self.pending.lock().unwrap().remove(response_event);
        match sender {
            Some(tx) => tx.send(payload).is_ok(),
            None => false,
        }
    }

    /// Names of the events currently being waited for.
    pub fn waiting_for(&self) -> Vec<String> {
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, tx| !tx.is_closed());
        pending.keys().cloned().collect()
    }
}

pub struct AppConfig {
    /// Persistent data (anisette state, pairing records, saved accounts).
    pub data_dir: PathBuf,
    /// Scratch space for downloaded IPAs.
    pub temp_dir: PathBuf,
    /// Key used to encrypt saved Apple ID passwords. `None` disables saving.
    pub credential_key: Option<[u8; 32]>,
}

/// Everything the original Tauri app kept in `AppHandle` / `State<...>`.
pub struct AppContext {
    pub config: AppConfig,
    pub events: Arc<dyn EventSink>,
    pub prompts: Arc<PromptHub>,
    pub sideloader: SideloaderMutex,
    pub device: DeviceInfoMutex,
    pub pairing_cancel: PairingCancelToken,
    pub storage: FileStorage,
    pub credentials: CredentialStore,
    saving_disabled: AtomicBool,
}

impl AppContext {
    pub fn new(config: AppConfig, events: Arc<dyn EventSink>) -> Result<Self, AppError> {
        storage::create_private_dir(&config.data_dir)?;
        std::fs::create_dir_all(&config.temp_dir).map_err(|e| {
            AppError::Filesystem("Failed to create temp dir".into(), e.to_string())
        })?;
        let storage = FileStorage::new(config.data_dir.join("storage"))?;
        let credentials =
            CredentialStore::new(config.data_dir.join("accounts.json"), config.credential_key);
        Ok(Self {
            config,
            events,
            prompts: Arc::new(PromptHub::new()),
            sideloader: SideloaderMutex::new(None),
            device: DeviceInfoMutex::new(None),
            pairing_cancel: PairingCancelToken::new(None),
            storage,
            credentials,
            saving_disabled: AtomicBool::new(false),
        })
    }

    /// Whether Apple ID passwords can be saved. This is what the UI calls
    /// "keyring available".
    pub fn credential_saving_available(&self) -> bool {
        !self.saving_disabled.load(Ordering::Relaxed) && self.credentials.has_key()
    }

    pub fn set_credential_saving_disabled(&self, disabled: bool) {
        self.saving_disabled.store(disabled, Ordering::Relaxed);
        if disabled {
            tracing::warn!("Saving credentials has been disabled by the user.");
        } else if !self.credentials.has_key() {
            tracing::warn!(
                "Saving credentials is not available: no encryption key configured on the server."
            );
        }
    }

    /// A boxed storage handle for isideload (anisette state, certificates).
    pub fn sideloading_storage(
        &self,
    ) -> Box<dyn isideload::util::storage::SideloadingStorage> {
        Box::new(self.storage.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prompt_delivery_and_timeout() {
        let hub = PromptHub::new();
        let fut = hub.register("2fa-recieved", Duration::from_secs(5));
        assert_eq!(hub.waiting_for(), vec!["2fa-recieved".to_string()]);
        assert!(hub.deliver("2fa-recieved", Value::String("123456".into())));
        assert_eq!(fut.await.unwrap(), Value::String("123456".into()));
        assert!(!hub.deliver("2fa-recieved", Value::Null));

        let fut = hub.register("max-certs-response", Duration::from_millis(20));
        assert!(matches!(fut.await, Err(PromptError::Timeout(_))));
    }
}
