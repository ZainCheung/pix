//! End-to-end harness: a real `pix serve` process, a real relay Worker
//! (wrangler dev), and a phone simulator implementing the same protocol
//! flows as the iOS app.
//!
//! These pieces exist to catch exactly the class of bugs that unit and
//! in-process integration tests miss: serve's stdin/stdout command loop,
//! event ordering, the relay bridge wiring, real WebSocket behavior of the
//! Durable Object, and multi-step pairing timing.

#![allow(dead_code)]
#![allow(
    clippy::assigning_clones,
    clippy::match_wild_err_arm,
    clippy::unused_self,
    clippy::large_enum_variant,
    clippy::manual_assert,
    clippy::missing_panics_doc
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use pix_wire::{
    ClientEnvelope, ClientRequest, EncryptedFrameDecoder, NoiseHandshake, NoisePattern,
    NoiseTransport, PROTOCOL_MAJOR, RelayRole, ServerEnvelope, ServerEvent, StaticKeyPair,
    decode_pairing_offer, encode_encrypted_frame, generate_static_keypair,
    pairing_introduction, relay_channel_id, relay_join_proof,
};
use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub const EVENT_TIMEOUT: Duration = Duration::from_secs(20);
const FRAME_TIMEOUT: Duration = Duration::from_secs(15);

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// ---------------------------------------------------------------------------
// Real `pix serve` process
// ---------------------------------------------------------------------------

pub struct Serve {
    child: Child,
    stdin: Option<ChildStdin>,
    events: mpsc::Receiver<Value>,
    /// Events received while waiting for something else; consumed first.
    buffered: Vec<Value>,
    pub config_path: PathBuf,
    pub port: u16,
    pub fingerprint: String,
}

impl Serve {
    /// Starts a fresh host in `config_dir`, optionally with a relay endpoint,
    /// and waits for its `ready` event.
    pub fn start(config_dir: &Path, relay_url: Option<&str>) -> Self {
        let config_path = config_dir.join("config.json");
        let pix = env!("CARGO_BIN_EXE_pix");

        // First start initializes the host; restarts reuse the existing
        // durable configuration, exactly like a real host process restart.
        if !config_path.exists() {
            let workspace = config_dir.join("workspace");
            std::fs::create_dir_all(&workspace).expect("workspace dir");
            run_pix(&config_path, &["workspace", "add", workspace.to_str().unwrap()]);
            if let Some(url) = relay_url {
                run_pix(&config_path, &["relay", "set", url]);
            }
        }

        let mut child = Command::new(pix)
            .args(["--config", config_path.to_str().unwrap(), "serve", "--json-events"])
            .env("PIX_DISABLE_KEYCHAIN", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn pix serve");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("serve stdout");
        let stderr = child.stderr.take().expect("serve stderr");

        let (events_tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && events_tx.send(value).is_err()
                {
                    break;
                }
            }
        });
        thread::spawn(move || {
            // Surface panics and aborts in test output.
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[serve stderr] {line}");
            }
        });

        let mut serve = Self {
            child,
            stdin,
            events,
            buffered: Vec::new(),
            config_path,
            port: 0,
            fingerprint: String::new(),
        };
        let ready = serve.wait_event("ready", |_| true);
        serve.port = u16::try_from(ready["port"].as_u64().expect("port")).expect("port range");
        serve.fingerprint = ready["fingerprint"].as_str().expect("fingerprint").to_owned();
        serve
    }

    pub fn lan_addr(&self) -> SocketAddr {
        format!("127.0.0.1:{}", self.port).parse().expect("lan addr")
    }

    pub fn command(&mut self, command: &str) {
        let stdin = self.stdin.as_mut().expect("serve stdin");
        writeln!(stdin, "{command}").expect("write serve command");
        stdin.flush().expect("flush serve command");
    }

    /// Waits for an event of `kind` matching `pred`. Events that arrive in
    /// the meantime are buffered, so waits are order-independent.
    pub fn wait_event(&mut self, kind: &str, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(position) = self
            .buffered
            .iter()
            .position(|event| event["type"].as_str() == Some(kind) && pred(event))
        {
            return self.buffered.remove(position);
        }
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                remaining > Duration::ZERO,
                "timed out waiting for serve event `{kind}` (buffered: {:?})",
                self.buffered
                    .iter()
                    .map(|event| event["type"].as_str().unwrap_or("?"))
                    .collect::<Vec<_>>()
            );
            match self.events.recv_timeout(remaining) {
                Ok(event) => {
                    if event["type"].as_str() == Some(kind) && pred(&event) {
                        return event;
                    }
                    self.buffered.push(event);
                }
                Err(_) => panic!("serve exited or stalled while waiting for `{kind}`"),
            }
        }
    }

    /// Drains buffered and pending events without blocking.
    pub fn drain_events(&mut self) -> Vec<Value> {
        let mut drained = std::mem::take(&mut self.buffered);
        while let Ok(event) = self.events.try_recv() {
            drained.push(event);
        }
        drained
    }

    /// Approves the next pairing request and returns its confirmation code.
    pub fn approve_next_pairing(&mut self) -> String {
        let request = self.wait_event("pairing_requested", |_| true);
        let id = request["id"].as_str().expect("request id").to_owned();
        let code = request["confirmation_code"]
            .as_str()
            .expect("confirmation code")
            .to_owned();
        self.command(&format!("approve {id}"));
        code
    }

    /// Requests the device list and waits until it has `count` entries.
    pub fn wait_devices(&mut self, count: usize) -> Vec<Value> {
        self.command("devices");
        let event = self.wait_event("device_list", |event| {
            event["devices"].as_array().is_some_and(|list| list.len() == count)
        });
        event["devices"].as_array().expect("device array").clone()
    }

    pub fn log_lines(&self) -> Vec<Value> {
        let path = self
            .config_path
            .parent()
            .expect("config dir")
            .join("logs/host.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    pub fn quit(&mut self) {
        if self.stdin.is_some() {
            self.command("quit");
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.child.try_wait().expect("child status").is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Closes stdout consumption (simulating a stalled/paused UI reader) by
    /// dropping the receiver end; serve must keep running regardless.
    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().expect("child status").is_none()
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_pix(config_path: &Path, args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_pix"))
        .arg("--config")
        .arg(config_path)
        .args(args)
        .output()
        .expect("run pix CLI");
    assert!(
        output.status.success(),
        "pix {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// Real relay Worker (wrangler dev)
// ---------------------------------------------------------------------------

pub struct Relay {
    child: Child,
    port: u16,
    pub url: String,
}

impl Relay {
    /// Starts `wrangler dev` on the given port and waits until it answers.
    pub fn start(port: u16) -> Self {
        // Reap any zombie from an earlier aborted test run.
        kill_port_listeners(port);

        let relay_dir = repo_root().join("relay");
        let mut command = Command::new("npx");
        command
            .args(["wrangler", "dev", "--port", &port.to_string()])
            .current_dir(&relay_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // npx spawns wrangler (node), which spawns and supervises workerd.
        // The whole tree must live in its own process group so stopping the
        // relay stops all of it; killing npx alone lets wrangler restart
        // workerd behind our back.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let child = command
            .spawn()
            .expect("spawn wrangler dev (is relay/node_modules installed?)");

        let relay = Self {
            child,
            port,
            url: format!("ws://127.0.0.1:{port}"),
        };
        relay.wait_until_ready(port);
        relay
    }

    fn wait_until_ready(&self, port: u16) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let request = format!(
                    "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                );
                if stream.write_all(request.as_bytes()).is_ok() {
                    let mut response = String::new();
                    let _ = stream.read_to_string(&mut response);
                    if response.contains("404") {
                        return;
                    }
                }
            }
            thread::sleep(Duration::from_millis(250));
        }
        panic!("wrangler dev did not become ready on port {port}");
    }

    pub fn stop(&mut self) {
        kill_group(&mut self.child);
        kill_port_listeners(self.port);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", self.port)).is_err() {
                return;
            }
            kill_port_listeners(self.port);
            thread::sleep(Duration::from_millis(200));
        }
        panic!("relay port {} is still serving after stop", self.port);
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        kill_group(&mut self.child);
        kill_port_listeners(self.port);
    }
}

/// Kills the child's whole process group, then reaps the child.
fn kill_group(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-9", &format!("-{}", child.id())])
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// Kills only processes LISTENING on the port; clients (like serve) that
/// merely have a connection to it must never be touched.
fn kill_port_listeners(port: u16) {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "lsof -ti tcp:{port} -s tcp:LISTEN | xargs kill -9 2>/dev/null"
        ))
        .status();
}

// ---------------------------------------------------------------------------
// Phone simulator
// ---------------------------------------------------------------------------

/// Remote pairing QR payload, parsed the same way the iOS app parses it.
pub struct PairPayload {
    pub relay_url: String,
    pub channel_secret: String,
    pub host_fingerprint: String,
}

pub fn parse_pair_payload(payload: &str) -> PairPayload {
    let query = payload
        .strip_prefix("pix://pair?")
        .expect("pix://pair payload");
    let mut relay_url = None;
    let mut secret = None;
    let mut fingerprint = None;
    let mut version = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').expect("query pair");
        match key {
            "v" => version = Some(value.to_owned()),
            "relay" => relay_url = Some(percent_decode(value)),
            "secret" => secret = Some(value.to_owned()),
            "fp" => fingerprint = Some(value.to_owned()),
            _ => {}
        }
    }
    assert_eq!(version.as_deref(), Some("1"), "payload version");
    PairPayload {
        relay_url: relay_url.expect("relay url"),
        channel_secret: secret.expect("channel secret"),
        host_fingerprint: fingerprint.expect("host fingerprint"),
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).expect("percent hex");
            decoded.push(u8::from_str_radix(hex, 16).expect("percent byte"));
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).expect("decoded utf8")
}

/// One byte link carrying length-prefixed encrypted records, LAN or relay.
pub enum Link {
    Lan {
        stream: TcpStream,
        decoder: EncryptedFrameDecoder,
        pending: Vec<Vec<u8>>,
    },
    Relay {
        socket: WebSocket<MaybeTlsStream<TcpStream>>,
        decoder: EncryptedFrameDecoder,
        pending: Vec<Vec<u8>>,
    },
}

impl Link {
    pub fn connect_lan(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).expect("connect host LAN listener");
        stream.set_nodelay(true).expect("nodelay");
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("read timeout");
        Self::Lan {
            stream,
            decoder: EncryptedFrameDecoder::new(),
            pending: Vec::new(),
        }
    }

    /// Joins the relay channel as `client` and waits for the host to be
    /// present, mirroring the iOS `RelayTransport` contract.
    pub fn connect_relay(relay_url: &str, channel_secret: &str) -> Result<Self, String> {
        let mut link = Self::join_relay(relay_url, channel_secret)?;
        link.wait_for_peer()?;
        Ok(link)
    }

    /// Joins as `client` without waiting for `peer_joined`. Used to race the
    /// host's local bridge open with the first Noise frame.
    pub fn join_relay(relay_url: &str, channel_secret: &str) -> Result<Self, String> {
        let channel = relay_channel_id(channel_secret).expect("channel id");
        let proof = relay_join_proof(channel_secret, RelayRole::Client).expect("client proof");
        Self::join_relay_raw(relay_url, &channel, &proof)
    }

    /// Attempts a relay join with caller-supplied identifiers. A 403 here is
    /// the Durable Object rejecting a stranger's proof.
    pub fn join_relay_raw(
        relay_url: &str,
        channel_id: &str,
        join_proof: &str,
    ) -> Result<Self, String> {
        let mut request = format!("{relay_url}/v1/channel/{channel_id}")
            .into_client_request()
            .expect("relay request");
        let headers = request.headers_mut();
        headers.insert("X-Pix-Protocol", "1".parse().unwrap());
        headers.insert("X-Pix-Role", "client".parse().unwrap());
        headers.insert("X-Pix-Join-Proof", join_proof.parse().unwrap());
        let (socket, _) =
            tungstenite::connect(request).map_err(|error| format!("join failed: {error}"))?;
        match socket.get_ref() {
            MaybeTlsStream::Plain(stream) => stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .expect("poll timeout"),
            _ => panic!("local relay is plain TCP"),
        }
        Ok(Self::Relay {
            socket,
            decoder: EncryptedFrameDecoder::new(),
            pending: Vec::new(),
        })
    }

    fn wait_for_peer(&mut self) -> Result<(), String> {
        let Self::Relay { socket, .. } = self else {
            return Ok(());
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match socket.read() {
                Ok(Message::Text(text)) if text.as_str().contains("peer_joined") => return Ok(()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(format!("relay link failed: {error}")),
            }
        }
        Err("timed out waiting for the host on the relay channel".to_owned())
    }

    pub fn send_record(&mut self, ciphertext: &[u8]) {
        match self {
            Self::Lan { stream, .. } => {
                let framed = encode_encrypted_frame(ciphertext).expect("frame");
                stream.write_all(&framed).expect("send LAN record");
            }
            Self::Relay { socket, .. } => {
                let framed = encode_encrypted_frame(ciphertext).expect("frame");
                socket
                    .send(Message::Binary(framed.into()))
                    .expect("send relay record");
            }
        }
    }

    pub fn read_record(&mut self, timeout: Duration) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + timeout;
        loop {
            match self {
                Self::Lan {
                    stream,
                    decoder,
                    pending,
                } => {
                    if !pending.is_empty() {
                        return Ok(pending.remove(0));
                    }
                    let mut chunk = [0_u8; 16 * 1024];
                    match stream.read(&mut chunk) {
                        Ok(0) => return Err("host closed the connection".to_owned()),
                        Ok(count) => {
                            pending.extend(decoder.push(&chunk[..count]).expect("decode records"));
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(error) => return Err(format!("LAN read failed: {error}")),
                    }
                }
                Self::Relay {
                    socket,
                    decoder,
                    pending,
                } => {
                    if !pending.is_empty() {
                        return Ok(pending.remove(0));
                    }
                    match socket.read() {
                        Ok(Message::Binary(frame)) => {
                            pending.extend(decoder.push(&frame).expect("decode records"));
                        }
                        Ok(Message::Text(text)) if text.as_str().contains("peer_left") => {
                            return Err("host left the relay channel".to_owned());
                        }
                        Ok(_) => {}
                        Err(tungstenite::Error::Io(error))
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(error) => return Err(format!("relay read failed: {error}")),
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for a record".to_owned());
            }
        }
    }
}

/// A simulated phone with a persistent identity and host trust, exercising
/// the same protocol steps as the iOS app.
pub struct Phone {
    pub keys: StaticKeyPair,
    pub host_public_key: Option<Vec<u8>>,
    pub relay_access: Option<(String, String)>,
}

impl Default for Phone {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PendingPairing {
    link: Link,
    transport: NoiseTransport,
    pub confirmation_code: String,
    host_public_key: Vec<u8>,
}

impl PendingPairing {
    /// The host static key revealed by XX, kept by the phone for IK probing
    /// when approval is interrupted.
    pub fn host_public_key_for_tests(&self) -> Vec<u8> {
        self.host_public_key.clone()
    }
}

pub struct Session {
    link: Link,
    transport: NoiseTransport,
    next_request_id: u64,
}

pub struct HostSnapshotInfo {
    pub display_name: String,
    pub relay: Option<(String, String)>,
}

impl Phone {
    pub fn new() -> Self {
        Self {
            keys: generate_static_keypair().expect("phone identity"),
            host_public_key: None,
            relay_access: None,
        }
    }

    /// Completes XX using a well-formed token that is not the one the host
    /// just issued. The host must reject the handshake instead of raising
    /// a pairing request.
    pub fn attempt_pairing_with_foreign_token(&self, mut link: Link, device_name: &str) {
        let mut handshake =
            NoiseHandshake::initiator(NoisePattern::PairingXx, &self.keys.private_key, None)
                .expect("phone XX");
        link.send_record(&handshake.write_message(b"").expect("XX message 1"));
        let message_2 = link.read_record(FRAME_TIMEOUT).expect("XX message 2");
        handshake
            .read_message(&message_2)
            .expect("XX message 2 payload");
        let foreign = random_channel_secret();
        link.send_record(
            &handshake
                .write_message(&pairing_introduction(&foreign, device_name).expect("introduction"))
                .expect("XX message 3"),
        );
        let closed = link.read_record(FRAME_TIMEOUT);
        assert!(
            closed.is_err(),
            "a foreign pairing token must close the handshake, got {closed:?}"
        );
    }

    /// Runs Noise XX up to the point where the host user must approve:
    /// message 1..3 exchanged, transport started, snapshot request sent.
    pub fn begin_pairing(&self, mut link: Link, device_name: &str) -> PendingPairing {
        let mut handshake =
            NoiseHandshake::initiator(NoisePattern::PairingXx, &self.keys.private_key, None)
                .expect("phone XX");
        link.send_record(&handshake.write_message(b"").expect("XX message 1"));
        let message_2 = link.read_record(FRAME_TIMEOUT).expect("XX message 2");
        let token = decode_pairing_offer(
            &handshake
                .read_message(&message_2)
                .expect("XX message 2 payload"),
        )
        .expect("pairing token");
        let host_public_key = handshake
            .remote_static()
            .expect("host static key")
            .to_vec();
        link.send_record(
            &handshake
                .write_message(&pairing_introduction(&token, device_name).expect("introduction"))
                .expect("XX message 3"),
        );
        let confirmation_code = pix_wire::confirmation_code(handshake.handshake_hash());
        let mut transport = handshake.into_transport().expect("phone transport");

        // The iOS app sends host.snapshot immediately; the response arrives
        // only after host approval.
        let request = ClientEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id: 1,
            request: ClientRequest::HostSnapshot,
        }
        .encode()
        .expect("encode snapshot request");
        for ciphertext in transport.encrypt_message(&request).expect("encrypt") {
            link.send_record(&ciphertext);
        }
        PendingPairing {
            link,
            transport,
            confirmation_code,
            host_public_key,
        }
    }

    /// Completes pairing after host approval and records durable trust.
    pub fn finish_pairing(&mut self, mut pending: PendingPairing) -> HostSnapshotInfo {
        let response = read_envelope(&mut pending.link, &mut pending.transport)
            .expect("approved host snapshot");
        let info = snapshot_info(&response);
        self.host_public_key = Some(pending.host_public_key);
        self.relay_access = info.relay.clone();
        info
    }

    /// Reconnects with IK and performs one host.snapshot round trip.
    pub fn reconnect(&mut self, link: Link) -> Result<(Session, HostSnapshotInfo), String> {
        let host_key = self.host_public_key.as_ref().expect("paired phone");
        let mut link = link;
        let mut handshake = NoiseHandshake::initiator(
            NoisePattern::ReconnectIk,
            &self.keys.private_key,
            Some(host_key),
        )
        .expect("phone IK");
        link.send_record(&handshake.write_message(b"").expect("IK message 1"));
        let message_2 = link.read_record(FRAME_TIMEOUT)?;
        handshake
            .read_message(&message_2)
            .map_err(|error| format!("IK message 2 rejected: {error}"))?;
        let mut session = Session {
            link,
            transport: handshake.into_transport().expect("IK transport"),
            next_request_id: 0,
        };
        let response = session.request(ClientRequest::HostSnapshot)?;
        let info = snapshot_info(&response);
        self.relay_access = info.relay.clone().or(self.relay_access.take());
        Ok((session, info))
    }
}

impl Session {
    pub fn request(&mut self, request: ClientRequest) -> Result<ServerEnvelope, String> {
        self.next_request_id += 1;
        let request_id = self.next_request_id;
        let encoded = ClientEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id,
            request,
        }
        .encode()
        .expect("encode request");
        for ciphertext in self.transport.encrypt_message(&encoded).expect("encrypt") {
            self.link.send_record(&ciphertext);
        }
        let deadline = Instant::now() + FRAME_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err("timed out waiting for the response".to_owned());
            }
            let envelope = read_envelope(&mut self.link, &mut self.transport)?;
            if envelope.request_id == Some(request_id) {
                if matches!(envelope.event, ServerEvent::RequestAck) {
                    continue;
                }
                return Ok(envelope);
            }
        }
    }
}

fn read_envelope(link: &mut Link, transport: &mut NoiseTransport) -> Result<ServerEnvelope, String> {
    loop {
        let record = link.read_record(FRAME_TIMEOUT)?;
        if let Some(plaintext) = transport
            .decrypt_record(&record)
            .map_err(|error| format!("decrypt failed: {error}"))?
        {
            return ServerEnvelope::decode(&plaintext)
                .map_err(|error| format!("decode failed: {error}"));
        }
    }
}

fn snapshot_info(envelope: &ServerEnvelope) -> HostSnapshotInfo {
    let ServerEvent::HostSnapshot { snapshot } = &envelope.event else {
        panic!("expected host snapshot, got {envelope:?}");
    };
    HostSnapshotInfo {
        display_name: snapshot.host.display_name.clone(),
        relay: snapshot
            .relay
            .as_ref()
            .map(|relay| (relay.url.clone(), relay.channel_secret.clone())),
    }
}

/// Extracts the QR payload from the `remote_pairing_ready` event.
pub fn qr_payload(event: &Value) -> String {
    event["qr_payload"].as_str().expect("qr payload").to_owned()
}

/// Extracts the typable join code from the `remote_pairing_ready` event.
pub fn join_code(event: &Value) -> String {
    event["join_code"].as_str().expect("join code").to_owned()
}

/// A random device-channel style secret for direct relay tests.
pub fn random_channel_secret() -> String {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
