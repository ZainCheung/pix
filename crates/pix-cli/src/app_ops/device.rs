//! Device operations that do not own terminal presentation.

#[cfg(unix)]
use std::io::{self, BufRead, BufReader};
use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::time::Duration;
#[cfg(unix)]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use pix_core::{ConfigStore, config::DeviceRecord};
use serde::Serialize;
use uuid::Uuid;

use crate::commands::shared::host_service_control_live;
use crate::{service, service_client};

/// A pending pairing request safe for a frontend to display.
///
/// Pairing offers and channel secrets are deliberately not part of this
/// value.  The confirmation code is the short value the user must compare
/// with the phone before approving a request.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PendingPairing {
    pub(crate) id: Uuid,
    pub(crate) device_name: String,
    pub(crate) confirmation_code: String,
    pub(crate) expires_at: u64,
}

impl std::fmt::Debug for PendingPairing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingPairing")
            .field("id", &self.id)
            .field("device_name", &self.device_name)
            .field("confirmation_code", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairingRequestAction {
    pub(crate) request_id: Uuid,
    pub(crate) action: PairingAction,
    /// Present when the request was selected by code/list and is useful to a
    /// text renderer.  An explicit request ID intentionally has only its ID,
    /// matching the existing JSON shape.
    pub(crate) request: Option<PendingPairing>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingAction {
    Approved,
    Rejected,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DeviceRevokeResult {
    pub(crate) device: DeviceRecord,
    pub(crate) service_cleanup: Option<ServiceCleanup>,
}

impl std::fmt::Debug for DeviceRevokeResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceRevokeResult")
            .field("device_id", &self.device.id)
            .field("device_name", &self.device.name)
            .field("paired_at", &self.device.paired_at)
            .field("service_cleanup", &self.service_cleanup)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ServiceCleanup {
    pub(crate) closed_connections: usize,
    pub(crate) connection_cleanup_failed: bool,
}

/// A short-lived value that is intentionally printable only through an
/// explicit pairing surface.  Its `Debug` implementation never reveals the
/// encoded payload, so accidental diagnostics cannot turn into a secret leak.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PairingSecret(String);

impl PairingSecret {
    fn new(value: String) -> Option<Self> {
        (!value.is_empty()).then_some(Self(value))
    }

    #[cfg(test)]
    pub(crate) fn from_test(value: &str) -> Self {
        Self(value.to_owned())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PairingSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PairingSecret([redacted])")
    }
}

/// A pairing offer intentionally contains secrets only for the caller that
/// is rendering the dedicated pairing surface.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PairingOffer {
    pub(crate) remote: bool,
    pub(crate) qr_payload: Option<PairingSecret>,
    pub(crate) join_code: Option<PairingSecret>,
    pub(crate) expires_at: Option<u64>,
}

impl std::fmt::Debug for PairingOffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingOffer")
            .field("remote", &self.remote)
            .field(
                "qr_payload",
                &self.qr_payload.as_ref().map(|_| "[redacted]"),
            )
            .field("join_code", &self.join_code.as_ref().map(|_| "[redacted]"))
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Safe, fixed-vocabulary pairing failures.  In particular, no event payload
/// or command error is carried through this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingFailure {
    ServiceUnavailable,
    InvalidEvent,
    RelayUnavailable,
    ConnectionFailed,
}

impl PairingFailure {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::ServiceUnavailable => "Pairing service unavailable",
            Self::InvalidEvent => "Pairing service returned an invalid event",
            Self::RelayUnavailable => "Remote pairing is unavailable",
            Self::ConnectionFailed => "Device connection failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PairingProgress {
    Waiting { offer: PairingOffer },
    OfferReady(PairingOffer),
    Request(PendingPairing),
    Approving,
    Denying,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PairingOutcome {
    Success { device_name: String },
    Denied,
    Cancelled,
    TimedOut,
    Expired,
    Error(PairingFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingCommand {
    Approve(Uuid),
    Deny(Uuid),
}

/// Lists paired devices without printing or prompting.
pub(crate) fn list_devices(store: &ConfigStore) -> Result<Vec<DeviceRecord>> {
    Ok(store.load().context("loading Pix configuration")?.devices)
}

/// Lists pending requests through the host control surface without printing.
pub(crate) fn list_pending(store: &ConfigStore) -> Result<Vec<PendingPairing>> {
    let event = service_client::request_event(
        store,
        "pending-list",
        "pairing_request_list",
        Duration::from_secs(5),
    )?;
    decode_pending_list(&event)
}

fn decode_pending_list(event: &serde_json::Value) -> Result<Vec<PendingPairing>> {
    let requests = event
        .get("requests")
        .and_then(serde_json::Value::as_array)
        .context("Pix host omitted pending pairing requests")?;
    requests
        .iter()
        .map(|value| pending_from_value(value).map_err(|error| anyhow!(error.message())))
        .collect::<Result<Vec<_>>>()
        .context("decoding pending pairing requests")
}

pub(crate) fn approve(store: &ConfigStore, request_id: Uuid) -> Result<PairingRequestAction> {
    complete_pairing_request(store, request_id, "approve", PairingAction::Approved)
}

pub(crate) fn deny(store: &ConfigStore, request_id: Uuid) -> Result<PairingRequestAction> {
    complete_pairing_request(store, request_id, "reject", PairingAction::Rejected)
}

fn complete_pairing_request(
    store: &ConfigStore,
    request_id: Uuid,
    verb: &str,
    action: PairingAction,
) -> Result<PairingRequestAction> {
    let event = service_client::request_event(
        store,
        &format!("{verb} {request_id}"),
        "pairing_request_handled",
        Duration::from_secs(5),
    )?;
    let expected_action = match action {
        PairingAction::Approved => "approved",
        PairingAction::Rejected => "rejected",
    };
    if event.get("request_id").and_then(serde_json::Value::as_str)
        != Some(request_id.to_string().as_str())
        || event.get("action").and_then(serde_json::Value::as_str) != Some(expected_action)
    {
        bail!("Pix host returned a mismatched pairing completion event")
    }
    Ok(PairingRequestAction {
        request_id,
        action,
        request: None,
    })
}

/// Revokes one already-confirmed device.  All service/config transaction
/// behavior is kept here so both CLI and TUI use exactly the same mutation.
pub(crate) fn revoke(store: &ConfigStore, device_id: &str) -> Result<DeviceRevokeResult> {
    let config = store.load().context("loading Pix configuration")?;
    let removed = config
        .devices
        .iter()
        .find(|device| device.id == device_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("unknown device: {device_id}"))?;

    let mut service_cleanup = None;
    if host_service_control_live(store)? {
        service_cleanup = Some(request_revoke(store, device_id)?);
    } else {
        let transaction = store.transaction()?;
        if host_service_control_live(store)? {
            drop(transaction);
            service_cleanup = Some(request_revoke(store, device_id)?);
        } else {
            let mut current = transaction
                .load()
                .context("loading current Pix configuration")?;
            let index = current
                .devices
                .iter()
                .position(|device| device.id == device_id)
                .ok_or_else(|| anyhow::anyhow!("unknown device: {device_id}"))?;
            current.devices.remove(index);
            transaction
                .save(&current)
                .context("saving Pix configuration")?;
        }
    }
    Ok(DeviceRevokeResult {
        device: removed,
        service_cleanup,
    })
}

fn request_revoke(store: &ConfigStore, device_id: &str) -> Result<ServiceCleanup> {
    let event = service_client::request_event(
        store,
        &format!("revoke {device_id}"),
        "device_revoked",
        Duration::from_secs(5),
    )?;
    if event.get("device_id").and_then(serde_json::Value::as_str) != Some(device_id) {
        bail!("Pix host returned a mismatched device revocation event")
    }
    Ok(ServiceCleanup {
        closed_connections: event
            .get("closed_connections")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0),
        connection_cleanup_failed: event
            .get("connection_cleanup_failed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// A host-local pairing wait.  The caller owns the worker thread and may
/// poll this value without ever writing to the terminal.
#[cfg(unix)]
pub(crate) struct PairingSession {
    store: ConfigStore,
    events: BufReader<std::os::unix::net::UnixStream>,
    remote: bool,
    deadline: Instant,
    expires_at: Option<u64>,
    request: Option<Uuid>,
    request_device_name: Option<String>,
}

#[cfg(unix)]
impl PairingSession {
    pub(crate) fn start(
        store: &ConfigStore,
        remote: bool,
    ) -> std::result::Result<Self, PairingFailure> {
        service::ensure_running(store).map_err(|_| PairingFailure::ServiceUnavailable)?;
        let stream =
            service::connect_events(store).map_err(|_| PairingFailure::ServiceUnavailable)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|_| PairingFailure::ServiceUnavailable)?;
        service::send_command(store, "pending").map_err(|_| PairingFailure::ServiceUnavailable)?;
        if remote {
            service::send_command(store, "pair-remote")
                .map_err(|_| PairingFailure::RelayUnavailable)?;
        }
        Ok(Self {
            store: store.clone(),
            events: BufReader::new(stream),
            remote,
            deadline: Instant::now() + Duration::from_secs(120),
            expires_at: None,
            request: None,
            request_device_name: None,
        })
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll(
        &mut self,
        cancel: &AtomicBool,
    ) -> std::result::Result<Option<PairingEvent>, PairingFailure> {
        if cancel.load(Ordering::Acquire) {
            self.cancel_pending();
            return Ok(Some(PairingEvent::Outcome(PairingOutcome::Cancelled)));
        }
        if Instant::now() >= self.deadline {
            self.cancel_pending();
            return Ok(Some(PairingEvent::Outcome(if self.remote {
                PairingOutcome::Expired
            } else {
                PairingOutcome::TimedOut
            })));
        }
        if self
            .expires_at
            .is_some_and(|expires_at| unix_now() >= expires_at)
        {
            self.cancel_pending();
            return Ok(Some(PairingEvent::Outcome(PairingOutcome::Expired)));
        }

        let mut line = String::new();
        match self.events.read_line(&mut line) {
            Ok(0) => return Err(PairingFailure::ServiceUnavailable),
            Ok(_) => {}
            Err(error) if is_timeout(&error) => return Ok(None),
            Err(_) => return Err(PairingFailure::ServiceUnavailable),
        }
        let value: serde_json::Value =
            serde_json::from_str(line.trim()).map_err(|_| PairingFailure::InvalidEvent)?;
        let event_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(PairingFailure::InvalidEvent)?;
        match event_type {
            "remote_pairing_ready" => {
                let payload = value
                    .get("qr_payload")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| PairingSecret::new(value.to_owned()))
                    .ok_or(PairingFailure::InvalidEvent)?;
                let join_code = value
                    .get("join_code")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| PairingSecret::new(value.to_owned()))
                    .ok_or(PairingFailure::InvalidEvent)?;
                let expires_at = value
                    .get("expires_at")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|expires_at| *expires_at > 0)
                    .ok_or(PairingFailure::InvalidEvent)?;
                self.expires_at = Some(expires_at);
                Ok(Some(PairingEvent::Progress(PairingProgress::OfferReady(
                    PairingOffer {
                        remote: true,
                        qr_payload: Some(payload),
                        join_code: Some(join_code),
                        expires_at: Some(expires_at),
                    },
                ))))
            }
            "pairing_requested" => {
                let request = pending_from_value(&value)?;
                self.request = Some(request.id);
                self.request_device_name = Some(request.device_name.clone());
                self.expires_at = (request.expires_at > 0).then_some(request.expires_at);
                Ok(Some(PairingEvent::Progress(PairingProgress::Request(
                    request,
                ))))
            }
            "connection_failed" if self.request.is_some() => Ok(Some(PairingEvent::Outcome(
                PairingOutcome::Error(PairingFailure::ConnectionFailed),
            ))),
            "relay_channel" => {
                let failed = value.get("label").and_then(serde_json::Value::as_str)
                    == Some("pairing")
                    && value
                        .get("state")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|state| state.starts_with("failed"));
                if failed {
                    Ok(Some(PairingEvent::Outcome(PairingOutcome::Error(
                        PairingFailure::RelayUnavailable,
                    ))))
                } else {
                    Ok(None)
                }
            }
            "command_error" => Ok(Some(PairingEvent::Outcome(PairingOutcome::Error(
                PairingFailure::ServiceUnavailable,
            )))),
            _ => Ok(None),
        }
    }

    pub(crate) fn approve(
        &mut self,
        request_id: Uuid,
    ) -> std::result::Result<String, PairingFailure> {
        if self.request != Some(request_id) {
            return Err(PairingFailure::InvalidEvent);
        }
        let device_name = self
            .request_device_name
            .clone()
            .ok_or(PairingFailure::InvalidEvent)?;
        crate::app_ops::device::approve(&self.store, request_id)
            .map_err(|_| PairingFailure::ServiceUnavailable)?;
        self.request = None;
        self.request_device_name = None;
        Ok(device_name)
    }

    pub(crate) fn deny(&mut self, request_id: Uuid) -> std::result::Result<(), PairingFailure> {
        if self.request != Some(request_id) {
            return Err(PairingFailure::InvalidEvent);
        }
        crate::app_ops::device::deny(&self.store, request_id)
            .map_err(|_| PairingFailure::ServiceUnavailable)?;
        self.request = None;
        self.request_device_name = None;
        Ok(())
    }

    /// Releases any ephemeral request or remote offer after a terminal error.
    /// Durable trust is only written by the host after an explicit approval.
    pub(crate) fn cancel(&mut self) {
        self.cancel_pending();
    }

    fn cancel_pending(&mut self) {
        if let Some(request) = self.request.take() {
            let _ = crate::app_ops::device::deny(&self.store, request);
        }
        self.request_device_name = None;
        if self.remote {
            let _ = service::send_command(&self.store, "pair-cancel");
        }
    }
}

#[cfg(not(unix))]
pub(crate) struct PairingSession;

#[cfg(not(unix))]
impl PairingSession {
    pub(crate) fn start(
        _store: &ConfigStore,
        _remote: bool,
    ) -> std::result::Result<Self, PairingFailure> {
        Err(PairingFailure::ServiceUnavailable)
    }

    pub(crate) fn poll(
        &mut self,
        _cancel: &AtomicBool,
    ) -> std::result::Result<Option<PairingEvent>, PairingFailure> {
        Err(PairingFailure::ServiceUnavailable)
    }

    pub(crate) fn approve(
        &mut self,
        _request_id: Uuid,
    ) -> std::result::Result<String, PairingFailure> {
        Err(PairingFailure::ServiceUnavailable)
    }

    pub(crate) fn deny(&mut self, _request_id: Uuid) -> std::result::Result<(), PairingFailure> {
        Err(PairingFailure::ServiceUnavailable)
    }

    pub(crate) fn cancel(&mut self) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PairingEvent {
    Progress(PairingProgress),
    Outcome(PairingOutcome),
}

/// Requests the explicit CLI remote offer through the versioned Host control
/// path. The TUI uses [`PairingSession`] for its event-driven lifecycle, but
/// headless commands retain replay/conflict and timeout semantics from the
/// existing `pairing.remote_offer` RPC.
pub(crate) fn remote_pairing_offer_rpc(store: &ConfigStore) -> Result<PairingOffer> {
    let event = service_client::request_event(
        store,
        "pair-remote",
        "remote_pairing_ready",
        Duration::from_secs(10),
    )?;
    let qr_payload = event
        .get("qr_payload")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| PairingSecret::new(value.to_owned()))
        .context("Pix host returned an invalid remote pairing QR payload")?;
    let join_code = event
        .get("join_code")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| PairingSecret::new(value.to_owned()))
        .context("Pix host returned an invalid remote pairing join code")?;
    let expires_at = event
        .get("expires_at")
        .and_then(serde_json::Value::as_u64)
        .filter(|expires_at| *expires_at > 0)
        .context("Pix host returned an invalid remote pairing expiry")?;
    Ok(PairingOffer {
        remote: true,
        qr_payload: Some(qr_payload),
        join_code: Some(join_code),
        expires_at: Some(expires_at),
    })
}

fn pending_from_value(
    value: &serde_json::Value,
) -> std::result::Result<PendingPairing, PairingFailure> {
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(PairingFailure::InvalidEvent)?;
    let device_name = value
        .get("device_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Your device")
        .to_owned();
    let confirmation_code = value
        .get("confirmation_code")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or(PairingFailure::InvalidEvent)?
        .to_owned();
    let expires_at = value
        .get("expires_at")
        .and_then(serde_json::Value::as_u64)
        .filter(|expires_at| *expires_at > 0)
        .ok_or(PairingFailure::InvalidEvent)?;
    Ok(PendingPairing {
        id,
        device_name,
        confirmation_code,
        expires_at,
    })
}

#[cfg(unix)]
fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

#[cfg(unix)]
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::{PairingFailure, decode_pending_list, pending_from_value};
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn pending_event_requires_a_canonical_confirmation_code_and_expiry() {
        let id = Uuid::new_v4();
        let valid = json!({
            "id": id,
            "device_name": "Phone",
            "confirmation_code": "012345",
            "expires_at": 1_900_000_000_u64,
        });
        let pending = pending_from_value(&valid).expect("valid pending event");
        assert_eq!(pending.id, id);
        assert_eq!(pending.confirmation_code, "012345");

        for invalid in [
            json!({
                "id": id,
                "device_name": "Phone",
                "expires_at": 1_900_000_000_u64,
            }),
            json!({
                "id": id,
                "device_name": "Phone",
                "confirmation_code": "",
                "expires_at": 1_900_000_000_u64,
            }),
            json!({
                "id": id,
                "device_name": "Phone",
                "confirmation_code": "12-3456",
                "expires_at": 1_900_000_000_u64,
            }),
            json!({
                "id": id,
                "device_name": "Phone",
                "confirmation_code": "123456",
                "expires_at": 0_u64,
            }),
        ] {
            assert_eq!(
                pending_from_value(&invalid),
                Err(PairingFailure::InvalidEvent)
            );
        }
    }

    #[test]
    fn pending_list_rejects_malformed_requests_before_the_devices_page() {
        let id = Uuid::new_v4();
        let event = json!({
            "requests": [{
                "id": id,
                "device_name": "Phone",
                "confirmation_code": "",
                "expires_at": 0_u64,
            }],
        });
        let error = decode_pending_list(&event).expect_err("malformed request must fail closed");
        assert!(format!("{error:#}").contains("invalid event"));
    }
}
