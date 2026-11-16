use std::{sync::Arc, time::Duration};

use futures::FutureExt;
use isideload::{
    anisette::{
        AnisetteClientInfo,
        remote_v3::{ANISETTE_STATE_KEY, RemoteV3AnisetteProvider},
    },
    auth::apple_account::{AppleAccount, TwoFactorCallbackParams, TwoFactorCallbackResponse},
    dev::{
        app_ids::{AppIdsApi, ListAppIdsResponse},
        certificates::{CertificatesApi, DevelopmentCertificate},
        developer_session::DeveloperSession,
    },
    sideload::{
        SideloaderBuilder,
        builder::{ExtensionsBehavior, ExtensionsBehaviorSelection, MaxCertsBehavior},
        sideloader::Sideloader,
    },
    util::{
        callbacks::{ExtensionsBehaviorCallbackBox, MaxCertsCallbackBox},
        storage::SideloadingStorage,
    },
};
use rootcause::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::{AppContext, error::AppError, sideload::SideloaderGuard};

pub const TWO_FACTOR_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_CERTS_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn login_new(
    ctx: &AppContext,
    email: String,
    password: String,
    anisette_server: String,
    save_credentials: bool,
) -> Result<(), AppError> {
    // fail before contacting Apple if the password could not be saved anyway
    if save_credentials && !ctx.credential_saving_available() {
        return Err(AppError::KeyringWithMessage(
            "Failed to save credentials".into(),
            "saving credentials is disabled on this server".into(),
        ));
    }
    let account = login(ctx, &email, &password, anisette_server).await?;
    *ctx.sideloader.lock().unwrap() = Some(account);

    if save_credentials {
        ctx.credentials.save_password(&email, &password)?;
    }
    Ok(())
}

pub async fn login_stored(
    ctx: &AppContext,
    email: String,
    anisette_server: String,
) -> Result<(), AppError> {
    let password = ctx.credentials.get_password(&email)?;
    let account = login(ctx, &email, &password, anisette_server).await?;
    *ctx.sideloader.lock().unwrap() = Some(account);
    Ok(())
}

/// Emails with a saved password (was the `ids` key of the Tauri `data.json` store).
pub fn saved_accounts(ctx: &AppContext) -> Result<Vec<String>, AppError> {
    ctx.credentials.list()
}

pub fn delete_account(ctx: &AppContext, email: String) -> Result<(), AppError> {
    ctx.credentials.delete(&email)
}

pub fn logged_in_as(ctx: &AppContext) -> Option<String> {
    ctx.sideloader
        .lock()
        .unwrap()
        .as_ref()
        .map(|account| account.get_email().to_string())
}

pub fn invalidate_account(ctx: &AppContext) {
    *ctx.sideloader.lock().unwrap() = None;
}

pub fn reset_anisette_state(ctx: &AppContext) -> Result<bool, AppError> {
    if !ctx.storage.exists(ANISETTE_STATE_KEY) {
        debug!("No existing anisette state found, nothing to delete.");
        return Ok(false);
    }
    ctx.storage.delete(ANISETTE_STATE_KEY).map_err(|e| {
        AppError::Storage("Failed to delete anisette state".into(), e.to_string())
    })?;
    debug!("Anisette state deleted.");
    Ok(true)
}

/// Turns the payload of a `2fa-recieved` event into the code. The Tauri front
/// end sent it as a JSON string; accept a bare/quoted string or a number too.
pub fn parse_two_factor_code(payload: &Value) -> String {
    match payload {
        Value::String(s) => s.trim().trim_matches('"').to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string().trim_matches('"').to_string(),
    }
}

async fn login(
    ctx: &AppContext,
    email: &str,
    password: &str,
    anisette_server: String,
) -> Result<Sideloader<MaxCertsCallbackBox, ExtensionsBehaviorCallbackBox>, AppError> {
    let tfa_closure = {
        let events = ctx.events.clone();
        let prompts = ctx.prompts.clone();
        move |params: TwoFactorCallbackParams| {
            let events = events.clone();
            let prompts = prompts.clone();
            async move {
                let payload =
                    serde_json::to_value(&params).context("Failed to encode 2FA params")?;
                let answer = prompts.register("2fa-recieved", TWO_FACTOR_TIMEOUT);
                events.emit("2fa-required", payload);
                let value = answer.await.context("No 2FA code received")?;
                Ok(TwoFactorCallbackResponse::SubmitCode(parse_two_factor_code(
                    &value,
                )))
            }
            .boxed()
        }
    };

    let anisette_url = if !anisette_server.starts_with("http") {
        format!("https://{}", anisette_server)
    } else {
        anisette_server
    };

    let mut account = AppleAccount::builder(&email.to_lowercase())
        .anisette_provider(
            RemoteV3AnisetteProvider::default()?
                .set_serial_number("0".to_string())
                .set_storage(ctx.sideloading_storage())
                // just temporary while waiting for the anisette servers to start reporting this
                .set_client_info_override(Some(AnisetteClientInfo {
                    client_info:
                        "<Mac15,7> <macOS;27.0;26A5378j> <com.apple.AuthKit/1 (com.apple.akd/1.0)>"
                            .to_string(),
                    user_agent: "akd/1.0 CFNetwork/811.5.4".to_string(),
                }))
                .set_url(&anisette_url),
        )
        .login(password, Box::new(tfa_closure))
        .await?;

    debug!("Logged in");

    let dev_session = DeveloperSession::from_account(&mut account).await?;

    debug!("Created developer session");

    let events = ctx.events.clone();
    let prompts: Arc<_> = ctx.prompts.clone();
    let max_certs_callback: MaxCertsCallbackBox =
        Box::new(move |certs: Vec<DevelopmentCertificate>| {
            let events = events.clone();
            let prompts = prompts.clone();
            Box::pin(async move {
                let cert_infos: Vec<CertificateInfo> =
                    certs.iter().map(CertificateInfo::from).collect();
                let payload =
                    serde_json::to_value(cert_infos).context("Failed to encode certificates")?;
                let answer = prompts.register("max-certs-response", MAX_CERTS_TIMEOUT);
                events.emit("max-certs-reached", payload);
                let value = answer.await.context("No certificate selection received")?;
                Ok(serde_json::from_value::<Option<Vec<String>>>(value).unwrap_or(None))
            })
        });

    // TODO: Team Selection

    let sideloader = SideloaderBuilder::new(dev_session, email.to_lowercase())
        .machine_name("iloader".into())
        .storage(ctx.sideloading_storage())
        .extensions_behavior(ExtensionsBehaviorSelection::Fixed(
            ExtensionsBehavior::ReuseMain,
        ))
        .max_certs_behavior(MaxCertsBehavior::Prompt(max_certs_callback))
        .build();

    debug!("Built sideloader");

    Ok(sideloader)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateInfo {
    pub name: Option<String>,
    pub certificate_id: Option<String>,
    pub serial_number: Option<String>,
    pub machine_name: Option<String>,
    pub machine_id: Option<String>,
}

impl From<&DevelopmentCertificate> for CertificateInfo {
    fn from(cert: &DevelopmentCertificate) -> Self {
        CertificateInfo {
            name: cert.name.clone(),
            certificate_id: cert.certificate_id.clone(),
            serial_number: cert.serial_number.clone(),
            machine_name: cert.machine_name.clone(),
            machine_id: cert.machine_id.clone(),
        }
    }
}

pub async fn get_certificates(ctx: &AppContext) -> Result<Vec<CertificateInfo>, AppError> {
    let mut sideloader = SideloaderGuard::take(&ctx.sideloader)?;

    let team = sideloader.get_mut().get_team().await?;
    let dev_session = sideloader.get_mut().get_dev_session();

    let certificates = dev_session.list_all_development_certs(&team, None).await?;

    Ok(certificates.iter().map(CertificateInfo::from).collect())
}

pub async fn revoke_certificate(ctx: &AppContext, serial_number: String) -> Result<(), AppError> {
    let mut sideloader = SideloaderGuard::take(&ctx.sideloader)?;

    let team = sideloader.get_mut().get_team().await?;
    let dev_session = sideloader.get_mut().get_dev_session();

    dev_session
        .revoke_development_cert(&team, &serial_number, None)
        .await?;

    Ok(())
}

pub async fn list_app_ids(ctx: &AppContext) -> Result<ListAppIdsResponse, AppError> {
    let mut sideloader = SideloaderGuard::take(&ctx.sideloader)?;

    let team = sideloader.get_mut().get_team().await?;
    let dev_session = sideloader.get_mut().get_dev_session();

    let response = dev_session.list_app_ids(&team, None).await?;

    Ok(response.clone())
}

pub async fn delete_app_id(ctx: &AppContext, app_id_id: String) -> Result<(), AppError> {
    let mut sideloader = SideloaderGuard::take(&ctx.sideloader)?;

    let team = sideloader.get_mut().get_team().await?;
    let dev_session = sideloader.get_mut().get_dev_session();

    dev_session.delete_app_id(&team, &app_id_id, None).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn two_factor_code_parsing() {
        assert_eq!(parse_two_factor_code(&json!("123456")), "123456");
        assert_eq!(parse_two_factor_code(&json!("\"123456\"")), "123456");
        assert_eq!(parse_two_factor_code(&json!(" 123456 ")), "123456");
        assert_eq!(parse_two_factor_code(&json!(123456)), "123456");
    }
}
