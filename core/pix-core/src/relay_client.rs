//! Outbound relay transport for the host.
//!
//! Each paired device gets one standing WebSocket to its rendezvous channel.
//! When the relay reports the device joined, the agent opens one loopback TCP
//! connection to the host's own listener and pumps opaque length-prefixed
//! ciphertext frames in both directions. The host service therefore treats a
//! relay client exactly like a LAN client: the same first-frame XX/IK
//! classification, pairing approval, revocation, and dispatcher loop apply,
//! and the Noise channel stays end-to-end between phone and host.
//!
//! This module never sees plaintext application data. Its events and errors
//! are payload-free by construction.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use pix_wire::{
    EncryptedFrameDecoder, MAX_ENCRYPTED_FRAME_BYTES, RelayRole, WireError, encode_encrypted_frame,
    relay_channel_id, relay_join_proof,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Protocol-level ping cadence. Cloudflare's edge closes idle `WebSockets`
/// well before two minutes, and a standing channel is silent between
/// sessions. The relay runtime answers pings without waking the channel.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(25);
/// Prefixed frame bytes: 4-byte length header plus the 1 MiB ciphertext cap.
const MAX_RELAY_MESSAGE_BYTES: usize = MAX_ENCRYPTED_FRAME_BYTES + 4;

/// Payload-free lifecycle events safe for host logs and native UIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayServiceEvent {
    /// The standing channel is connected to the relay and waiting for a peer.
    ChannelWaiting {
        label: String,
    },
    PeerJoined {
        label: String,
    },
    PeerLeft {
        label: String,
    },
    ChannelFailed {
        label: String,
        stage: RelayStage,
        /// Human-readable error cause. Derived from transport errors only;
        /// never contains channel secrets, proofs, or application data.
        detail: String,
    },
    ChannelStopped {
        label: String,
    },
}

/// Coarse failure stage without addresses, tokens, or payload data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayStage {
    Connect,
    Join,
    Transport,
    LocalBridge,
}

#[derive(Debug, Error)]
pub enum RelayClientError {
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error("relay URL is invalid")]
    InvalidUrl,
    #[error("relay TCP connection failed: {0}")]
    Connect(std::io::Error),
    #[error("relay WebSocket handshake failed: {0}")]
    WebSocket(String),
    #[error("relay transport failed: {0}")]
    Transport(Box<tungstenite::Error>),
    #[error("relay sent an invalid control or frame message")]
    Protocol,
    #[error("local host listener bridge failed: {0}")]
    LocalBridge(std::io::Error),
    #[error("failed to start relay thread: {0}")]
    Spawn(std::io::Error),
}

impl From<tungstenite::Error> for RelayClientError {
    fn from(error: tungstenite::Error) -> Self {
        Self::Transport(Box::new(error))
    }
}

/// Configuration for one standing device channel.
#[derive(Clone)]
pub struct RelayChannelConfig {
    /// Base relay endpoint, `wss://relay.example.com` or `ws://host:port`.
    pub relay_url: String,
    /// The device's canonical base64url channel secret.
    pub channel_secret: String,
    /// Port of the host's own LAN listener on 127.0.0.1.
    pub local_port: u16,
    /// Short payload-free label used in events, never the secret itself.
    pub label: String,
}

/// One supervised standing channel with reconnection and backoff.
struct RelayAgent {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RelayAgent {
    fn start(
        config: RelayChannelConfig,
        events: mpsc::Sender<RelayServiceEvent>,
        deadline: Option<Instant>,
    ) -> Result<Self, RelayClientError> {
        // Fail fast on malformed secrets before spawning a supervisor.
        relay_channel_id(&config.channel_secret)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name(format!("pix-relay-{}", config.label))
            .spawn(move || supervise(&config, &events, &thread_stop, deadline))
            .map_err(RelayClientError::Spawn)?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RelayAgent {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn supervise(
    config: &RelayChannelConfig,
    events: &mpsc::Sender<RelayServiceEvent>,
    stop: &AtomicBool,
    deadline: Option<Instant>,
) {
    let mut backoff = Backoff::new();
    while !stop.load(Ordering::Acquire) && deadline.is_none_or(|at| Instant::now() < at) {
        let started = Instant::now();
        let outcome = run_channel(config, events, stop, deadline);
        match outcome {
            Ok(_) => {
                // A healthy standing connection resets backoff so a relay
                // restart does not permanently slow reconnection.
                if started.elapsed() >= BACKOFF_INITIAL {
                    backoff.reset();
                }
            }
            Err(error) => {
                let _ = events.send(RelayServiceEvent::ChannelFailed {
                    label: config.label.clone(),
                    stage: error.stage(),
                    detail: error.to_string(),
                });
            }
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        sleep_with_stop(backoff.next_delay(), stop);
    }
    let _ = events.send(RelayServiceEvent::ChannelStopped {
        label: config.label.clone(),
    });
}

enum ChannelOutcome {
    /// The relay connection closed without a peer session.
    Idle,
    /// At least one peer session was bridged to the local listener.
    PeerServed,
}

impl RelayClientError {
    const fn stage(&self) -> RelayStage {
        match self {
            Self::Connect(_) | Self::InvalidUrl | Self::Wire(_) => RelayStage::Connect,
            Self::WebSocket(_) => RelayStage::Join,
            Self::Transport(_) | Self::Protocol | Self::Spawn(_) => RelayStage::Transport,
            Self::LocalBridge(_) => RelayStage::LocalBridge,
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run_channel(
    config: &RelayChannelConfig,
    events: &mpsc::Sender<RelayServiceEvent>,
    stop: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<ChannelOutcome, RelayClientError> {
    let mut socket = join_channel(config, RelayRole::Host)?;
    // A replaced or stopped agent must never present itself as a live host:
    // the stop flag may have been set while the join was in flight, and a
    // phone scanning a stale QR code would otherwise find a half-dead host
    // and hang mid-handshake.
    if stop.load(Ordering::Acquire) {
        let _ = socket.close(None);
        return Ok(ChannelOutcome::Idle);
    }
    let _ = events.send(RelayServiceEvent::ChannelWaiting {
        label: config.label.clone(),
    });

    let mut bridge: Option<Bridge> = None;
    let mut served = false;
    // Set when the first ciphertext arrived before `peer_joined`. A later
    // `peer_joined` for that same client must keep the bridge; a later
    // `peer_joined` after a finished session is a supersession and must
    // replace it.
    let mut early_frame_bridge = false;
    let mut last_ping = Instant::now();
    let outcome = loop {
        // The deadline closes the join window; a session that is already
        // bridged (pairing in progress) is allowed to finish. The pairing
        // token's own two-minute expiry still bounds the whole exchange.
        if stop.load(Ordering::Acquire)
            || (bridge.is_none() && deadline.is_some_and(|at| Instant::now() >= at))
        {
            break served_outcome(served);
        }

        // Both directions of the relay path idle while the user walks to
        // the Mac to approve; without keepalives the edge drops the socket.
        if last_ping.elapsed() >= KEEPALIVE_INTERVAL {
            socket.send(Message::Ping(Vec::new().into()))?;
            last_ping = Instant::now();
        }

        // Local listener bytes flow out first so responses are not delayed a
        // poll interval behind reads.
        if let Some(active) = bridge.as_mut() {
            let mut bridge_down = false;
            loop {
                match active.outbound.try_recv() {
                    Ok(frame) => socket.send(Message::Binary(frame.into()))?,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        bridge_down = true;
                        break;
                    }
                }
            }
            if bridge_down {
                // The host side closed this client's socket (revocation,
                // rejection, or dispatcher exit). Drop the relay connection
                // too so the peer observes a definite close instead of a
                // half-open channel.
                drop(bridge.take());
                break served_outcome(served);
            }
        }

        match socket.read() {
            Ok(Message::Text(control)) => match parse_control(control.as_ref()) {
                Some(RelayControl::PeerJoined) => {
                    if stop.load(Ordering::Acquire) {
                        break served_outcome(served);
                    }
                    let _ = events.send(RelayServiceEvent::PeerJoined {
                        label: config.label.clone(),
                    });
                    served = true;
                    if early_frame_bridge && bridge.is_some() {
                        early_frame_bridge = false;
                    } else {
                        drop(bridge.take());
                        bridge = Some(Bridge::open(config.local_port)?);
                        early_frame_bridge = false;
                    }
                }
                Some(RelayControl::PeerLeft) => {
                    let _ = events.send(RelayServiceEvent::PeerLeft {
                        label: config.label.clone(),
                    });
                    bridge = None;
                    early_frame_bridge = false;
                }
                None => return Err(RelayClientError::Protocol),
            },
            Ok(Message::Binary(frame)) => {
                if frame.len() > MAX_RELAY_MESSAGE_BYTES {
                    return Err(RelayClientError::Protocol);
                }
                if bridge.is_none() {
                    // The phone may send its first Noise frame in the same
                    // instant the relay announces `peer_joined`. Opening the
                    // local bridge on that frame (as well as on the control
                    // message) keeps the handshake from being dropped.
                    if stop.load(Ordering::Acquire) {
                        break served_outcome(served);
                    }
                    served = true;
                    let _ = events.send(RelayServiceEvent::PeerJoined {
                        label: config.label.clone(),
                    });
                    bridge = Some(Bridge::open(config.local_port)?);
                    early_frame_bridge = true;
                }
                if let Some(active) = bridge.as_mut()
                    && active.write(&frame).is_err()
                {
                    drop(bridge.take());
                    break served_outcome(served);
                }
            }
            Ok(Message::Ping(payload)) => socket.send(Message::Pong(payload))?,
            Ok(Message::Close(_)) => break served_outcome(served),
            Ok(Message::Pong(_) | Message::Frame(_)) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                break served_outcome(served);
            }
            Err(error) => return Err(error.into()),
        }
    };
    let _ = socket.close(None);
    Ok(outcome)
}

const fn served_outcome(served: bool) -> ChannelOutcome {
    if served {
        ChannelOutcome::PeerServed
    } else {
        ChannelOutcome::Idle
    }
}

/// Installs the process-level rustls crypto provider exactly once.
///
/// rustls 0.23 panics inside the TLS handshake when no default provider was
/// installed, and the plaintext `ws://` test path never exercises TLS, so
/// this must run before any `wss://` connection attempt.
fn ensure_tls_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Connects, upgrades, and joins one rendezvous channel as the given role.
fn join_channel(
    config: &RelayChannelConfig,
    role: RelayRole,
) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, RelayClientError> {
    ensure_tls_crypto_provider();
    let channel_id = relay_channel_id(&config.channel_secret)?;
    let proof = relay_join_proof(&config.channel_secret, role)?;
    let endpoint = RelayEndpoint::parse(&config.relay_url)?;
    let url = format!(
        "{}/v1/channel/{channel_id}",
        config.relay_url.trim_end_matches('/')
    );

    let address = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map_err(RelayClientError::Connect)?
        .next()
        .ok_or(RelayClientError::InvalidUrl)?;
    let stream =
        TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(RelayClientError::Connect)?;
    stream
        .set_nodelay(true)
        .map_err(RelayClientError::Connect)?;
    stream
        .set_read_timeout(Some(WS_HANDSHAKE_TIMEOUT))
        .map_err(RelayClientError::Connect)?;

    let mut request = url
        .into_client_request()
        .map_err(|_| RelayClientError::InvalidUrl)?;
    let headers = request.headers_mut();
    headers.insert("X-Pix-Protocol", HeaderValue::from_static("1"));
    headers.insert(
        "X-Pix-Role",
        HeaderValue::from_static(match role {
            RelayRole::Host => "host",
            RelayRole::Client => "client",
        }),
    );
    headers.insert(
        "X-Pix-Join-Proof",
        HeaderValue::from_str(&proof).map_err(|_| RelayClientError::InvalidUrl)?,
    );

    let (socket, _response) = tungstenite::client_tls_with_config(request, stream, None, None)
        .map_err(|error| RelayClientError::WebSocket(error.to_string()))?;
    set_stream_read_timeout(&socket, POLL_INTERVAL)?;
    Ok(socket)
}

struct RelayEndpoint {
    host: String,
    port: u16,
}

impl RelayEndpoint {
    fn parse(url: &str) -> Result<Self, RelayClientError> {
        let (scheme, rest) = url.split_once("://").ok_or(RelayClientError::InvalidUrl)?;
        let default_port = match scheme {
            "wss" => 443,
            "ws" => 80,
            _ => return Err(RelayClientError::InvalidUrl),
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.is_empty() {
            return Err(RelayClientError::InvalidUrl);
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => (
                host.to_owned(),
                port.parse().map_err(|_| RelayClientError::InvalidUrl)?,
            ),
            _ => (authority.to_owned(), default_port),
        };
        if host.is_empty() {
            return Err(RelayClientError::InvalidUrl);
        }
        Ok(Self { host, port })
    }
}

fn set_stream_read_timeout(
    socket: &WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Duration,
) -> Result<(), RelayClientError> {
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => stream.get_ref(),
        _ => return Err(RelayClientError::InvalidUrl),
    };
    stream
        .set_read_timeout(Some(timeout))
        .map_err(RelayClientError::Connect)
}

enum RelayControl {
    PeerJoined,
    PeerLeft,
}

fn parse_control(text: &str) -> Option<RelayControl> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("peer_joined") => Some(RelayControl::PeerJoined),
        Some("peer_left") => Some(RelayControl::PeerLeft),
        _ => None,
    }
}

/// One loopback TCP connection into the host's own listener.
struct Bridge {
    stream: TcpStream,
    outbound: mpsc::Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
}

impl Bridge {
    fn open(local_port: u16) -> Result<Self, RelayClientError> {
        let stream = TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, local_port)))
            .map_err(RelayClientError::LocalBridge)?;
        stream
            .set_nodelay(true)
            .map_err(RelayClientError::LocalBridge)?;
        let reader_stream = stream.try_clone().map_err(RelayClientError::LocalBridge)?;
        let (frames_tx, frames_rx) = mpsc::channel();
        let reader = thread::Builder::new()
            .name("pix-relay-bridge-read".to_owned())
            .spawn(move || pump_local_frames(reader_stream, &frames_tx))
            .map_err(RelayClientError::Spawn)?;
        Ok(Self {
            stream,
            outbound: frames_rx,
            reader: Some(reader),
        })
    }

    fn write(&mut self, frame: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(frame)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Reads the loopback stream and re-frames it into relay-sized records.
fn pump_local_frames(mut stream: TcpStream, frames: &mpsc::Sender<Vec<u8>>) {
    let mut decoder = EncryptedFrameDecoder::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let count = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        let Ok(records) = decoder.push(&chunk[..count]) else {
            break;
        };
        for ciphertext in records {
            let Ok(record) = encode_encrypted_frame(&ciphertext) else {
                break;
            };
            if frames.send(record).is_err() {
                return;
            }
        }
    }
    // Dropping the sender signals the channel loop that this bridge ended.
}

fn sleep_with_stop(total: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if stop.load(Ordering::Acquire) {
            return;
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Exponential backoff with random jitter, capped at one minute.
struct Backoff {
    next: Duration,
}

impl Backoff {
    const fn new() -> Self {
        Self {
            next: BACKOFF_INITIAL,
        }
    }

    fn reset(&mut self) {
        self.next = BACKOFF_INITIAL;
    }

    fn next_delay(&mut self) -> Duration {
        let base = self.next;
        self.next = (self.next * 2).min(BACKOFF_MAX);
        // Jitter in [50%, 150%] keeps reconnect storms decorrelated without
        // ever returning zero.
        let mut random = [0_u8; 4];
        let jitter_percent = if getrandom::fill(&mut random).is_ok() {
            50 + u64::from(u32::from_le_bytes(random)) % 101
        } else {
            100
        };
        base.saturating_mul(u32::try_from(jitter_percent).unwrap_or(100)) / 100
    }
}

/// Owns the standing per-device channels plus at most one pairing channel.
pub struct RelayManager {
    relay_url: String,
    local_port: u16,
    events: mpsc::Sender<RelayServiceEvent>,
    devices: Mutex<HashMap<String, RelayAgent>>,
    pairing: Mutex<Option<RelayAgent>>,
}

/// A short-lived remote pairing channel offer for QR presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePairingOffer {
    /// Canonical base64url channel secret embedded in the QR payload.
    pub channel_secret: String,
    /// Typable `XXXX-XXXX` join code that derives [`Self::channel_secret`].
    pub join_code: String,
    pub expires_in: Duration,
}

impl RelayManager {
    #[must_use]
    pub fn new(
        relay_url: impl Into<String>,
        local_port: u16,
        events: mpsc::Sender<RelayServiceEvent>,
    ) -> Self {
        Self {
            relay_url: relay_url.into(),
            local_port,
            events,
            devices: Mutex::new(HashMap::new()),
            pairing: Mutex::new(None),
        }
    }

    /// Reconciles standing channels with the durable paired-device list.
    ///
    /// New devices gain a standing channel; removed devices lose theirs. The
    /// channel secret never appears in events, thread names, or errors.
    ///
    /// # Errors
    ///
    /// Returns [`RelayClientError`] when a channel secret is malformed or a
    /// supervisor thread cannot start. Already-running channels keep running.
    pub fn sync_devices<'a>(
        &self,
        devices: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<(), RelayClientError> {
        let mut desired: HashMap<String, String> = HashMap::new();
        for (device_id, channel_secret) in devices {
            desired.insert(device_id.to_owned(), channel_secret.to_owned());
        }
        let mut agents = self
            .devices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        agents.retain(|device_id, _| desired.contains_key(device_id));
        for (device_id, channel_secret) in desired {
            if agents.contains_key(&device_id) {
                continue;
            }
            let agent = RelayAgent::start(
                RelayChannelConfig {
                    relay_url: self.relay_url.clone(),
                    channel_secret,
                    local_port: self.local_port,
                    label: label_for_device(&device_id),
                },
                self.events.clone(),
                None,
            )?;
            agents.insert(device_id, agent);
        }
        Ok(())
    }

    /// Starts a short-lived remote pairing channel and returns the secret
    /// for QR encoding. Any previous pairing channel is replaced.
    ///
    /// The TTL closes the join window; a pairing session already in progress
    /// is allowed to finish, and an interrupted phone may rejoin within the
    /// window to probe the approval outcome. Actual pairing authority stays
    /// with the single-use two-minute token plus explicit host approval.
    ///
    /// # Errors
    ///
    /// Returns [`RelayClientError`] when secure randomness is unavailable or
    /// the supervisor thread cannot start.
    pub fn start_remote_pairing(
        &self,
        ttl: Duration,
    ) -> Result<RemotePairingOffer, RelayClientError> {
        let join_code = pix_wire::generate_join_code()?;
        let channel_secret =
            pix_wire::relay_channel_secret_from_join_code(&join_code, &self.relay_url)?;
        let agent = RelayAgent::start(
            RelayChannelConfig {
                relay_url: self.relay_url.clone(),
                channel_secret: channel_secret.clone(),
                local_port: self.local_port,
                label: "pairing".to_owned(),
            },
            self.events.clone(),
            Some(Instant::now() + ttl),
        )?;
        let previous = self
            .pairing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(agent);
        if let Some(previous) = previous {
            // Shutdown joins the supervisor thread, which may be blocked in
            // a TLS connect for many seconds; never stall the caller (the
            // host command loop) on that.
            let _ = thread::Builder::new()
                .name("pix-relay-retire".to_owned())
                .spawn(move || drop(previous));
        }
        Ok(RemotePairingOffer {
            channel_secret,
            join_code,
            expires_in: ttl,
        })
    }

    /// Stops every standing and pairing channel.
    pub fn shutdown(&self) {
        self.devices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.pairing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

impl Drop for RelayManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn label_for_device(device_id: &str) -> String {
    device_id.chars().take(8).collect()
}
