//! Host-side primitives for Pix.
//!
//! Conversation content deliberately does not belong in this crate's durable
//! configuration. Pi's native JSONL session remains the source of truth.

pub mod config;
pub mod connection_manager;
pub mod diagnostics;
pub mod direct_tcp;
pub mod discovery;
pub mod host_dispatcher;
pub mod host_environment;
pub mod host_identity;
pub mod host_service;
pub mod image_assets;
pub mod pairing;
pub mod pi;
pub mod pi_bridge;
pub mod pi_defaults;
pub mod pi_rpc;
pub mod relay_client;
pub mod runtime;
pub mod runtime_manager;
pub mod secure_connection;
pub mod session;
pub mod session_history;
pub mod session_lock;
pub mod workspace;

pub use config::{ConfigStore, HostConfig};
pub use connection_manager::{ConnectionId, ConnectionRegistry, RequestAdmission, RequestLedger};
pub use diagnostics::install_sink as install_diagnostic_sink;
pub use direct_tcp::{ConnectionControl, DirectTcpListener, EncryptedConnection};
pub use discovery::{BonjourAdvertisement, BonjourMetadata, LanEndpoint, LanEndpointError};
pub use host_dispatcher::{DispatchError, HostProtocolDispatcher, HostState};
pub use host_environment::{EnvironmentSource, HostEnvironment};
pub use host_identity::{HostIdentityError, HostIdentityKey, HostIdentityStore};
pub use host_service::{
    ConfigRefreshReport, ConnectionStage, HostService, HostServiceError, HostServiceEvent,
    HostServiceHandle, PairingRequest,
};
pub use image_assets::{ImageAsset, ImageAssetChunk, ImageAssetError, ImageAssetStore};
pub use pairing::{
    ApprovedDevice, DeviceRevocation, MAX_PENDING_PAIRING_OFFERS, PairingCoordinator, PairingOffer,
    PairingPending, PairingToken,
};
pub use pi::{PiInstallation, PiProbe};
pub use pi_rpc::{PiCommand, PiEvent, PiResponse, RpcClient};
pub use pix_wire::host_public_key_fingerprint;
pub use relay_client::{
    RelayClientError, RelayManager, RelayServiceEvent, RelayStage, RemotePairingOffer,
    validate_relay_url,
};
pub use runtime::{PiRuntime, PiRuntimeOptions, SessionLaunch};
pub use runtime_manager::{ActiveRuntimeSummary, RuntimeManager, RuntimeManagerOptions};
pub use secure_connection::{AuthenticatedConnection, PendingPairingConnection};
pub use session::{
    DiscoveredSession, PiSessionStore, SessionListTiming, SessionMetadataIndex, SessionSnapshot,
    SessionSummary,
};
pub use session_lock::{SessionId, SessionLease};
pub use workspace::WorkspaceRegistry;
