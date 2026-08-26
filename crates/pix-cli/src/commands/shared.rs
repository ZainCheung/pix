//! Helpers shared across command groups: service mutations, terminal
//! formatting, identity loading, and small display utilities.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use pix_core::{ConfigStore, HostIdentityStore};

pub(crate) const DEFAULT_RELAY_URL: &str = "wss://pix-relay.zaincheung-255.workers.dev";

pub(crate) fn load_host_identity(
    store: &ConfigStore,
    host_id: uuid::Uuid,
) -> Result<pix_core::host_identity::HostIdentityKey> {
    load_host_identity_with_keychain_policy(store, host_id, true)
}

pub(crate) fn load_host_identity_for_service(
    store: &ConfigStore,
    host_id: uuid::Uuid,
) -> Result<pix_core::host_identity::HostIdentityKey> {
    load_host_identity_with_keychain_policy(store, host_id, false)
}

fn load_host_identity_with_keychain_policy(
    store: &ConfigStore,
    host_id: uuid::Uuid,
    allow_keychain_user_interaction: bool,
) -> Result<pix_core::host_identity::HostIdentityKey> {
    let identity_path = store
        .path()
        .parent()
        .context("locating host identity directory")?
        .join("host-identity.key");
    let identity_store = HostIdentityStore::new(identity_path);
    #[cfg(target_os = "macos")]
    let identity_store = if std::env::var("PIX_DISABLE_KEYCHAIN").is_ok_and(|value| value == "1") {
        identity_store
    } else {
        let identity_store = identity_store.with_keychain_host_id(host_id.to_string());
        if allow_keychain_user_interaction {
            identity_store
        } else {
            identity_store.without_keychain_user_interaction()
        }
    };
    #[cfg(target_os = "linux")]
    let identity_store = identity_store.with_secret_service_host_id(host_id.to_string());
    identity_store.load_or_create().map_err(Into::into)
}

pub(crate) fn format_confirmation_code(code: &str) -> String {
    if code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}

pub(crate) fn terminal_label(value: &str) -> String {
    let mut label = value
        .chars()
        .filter(|character| {
            !character.is_control()
                && !matches!(
                    *character,
                    '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200b}'..='\u{200f}'
                )
        })
        .take(80)
        .collect::<String>();
    if label.is_empty() {
        label.push_str("device");
    }
    label
}

pub(crate) fn expand_home(path: PathBuf) -> PathBuf {
    if path == std::path::Path::new("~") {
        return home_directory().unwrap_or(path);
    }
    if let Some(rest) = path.to_str().and_then(|value| value.strip_prefix("~/")) {
        return home_directory().map(|home| home.join(rest)).unwrap_or(path);
    }
    path
}

pub(crate) fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub(crate) fn display_workspace_path(path: &std::path::Path) -> String {
    if let Some(home) = home_directory()
        && let Ok(relative) = path.strip_prefix(&home)
    {
        if relative.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return format!("~/{}", relative.display());
    }
    path.display().to_string()
}

pub(crate) fn restart_or_stop_for_configuration_change(store: &ConfigStore) -> Result<()> {
    if service::managed_service_installed(store)? {
        service::restart_for_config(store)
    } else {
        // A manually launched host cannot be safely hot-swapped. Stop it so
        // disabled or replaced relay channels cease immediately; the next
        // pairing/start action will launch from the committed configuration.
        service::stop_quiet(store)
    }
}

pub(crate) fn refresh_running_service(store: &ConfigStore) -> Result<Option<serde_json::Value>> {
    if !host_service_control_live(store)? {
        return Ok(None);
    }
    let response = service_client::request_event(
        store,
        "refresh",
        "config_refreshed",
        Duration::from_secs(5),
    )?;
    Ok(Some(response))
}

pub(crate) fn host_service_control_live(store: &ConfigStore) -> Result<bool> {
    status::control_socket_live(store.path())
}

pub(crate) fn prepare_running_service_mutation(store: &ConfigStore) -> Result<()> {
    if host_service_control_live(store)? {
        service_client::verify_control_compatibility(store).context(
            "the running Pix host cannot safely apply this configuration mutation; restart it and retry",
        )?;
    }
    Ok(())
}

pub(crate) fn short_id(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

pub(crate) fn default_host_name() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Pix Host".to_owned())
}

pub(crate) fn load_or_ephemeral_config(store: &ConfigStore) -> Result<pix_core::HostConfig> {
    match store.load() {
        Ok(config) => Ok(config),
        Err(pix_core::config::ConfigError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(pix_core::HostConfig::new(default_host_name()))
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) const fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

use crate::{service, service_client, status};
