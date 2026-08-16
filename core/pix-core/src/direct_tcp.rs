use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use pix_wire::{EncryptedFrameDecoder, WireError, encode_encrypted_frame};
use thiserror::Error;

/// LAN listener for length-prefixed encrypted Pix frames.
///
/// This layer cannot encode or decode application messages. A caller must run
/// the Noise handshake and authentication before handing decrypted content to
/// the host protocol dispatcher.
pub struct DirectTcpListener {
    listener: TcpListener,
}

impl DirectTcpListener {
    /// Binds a LAN listener. Port zero requests an ephemeral port for tests or
    /// dynamic Bonjour advertisement.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] when the socket cannot be created.
    pub fn bind(port: u16) -> Result<Self, DirectTcpError> {
        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))
            .map_err(DirectTcpError::Bind)?;
        Ok(Self { listener })
    }

    /// Wraps a caller-supplied listener, primarily for controlled embedding.
    #[must_use]
    pub const fn from_listener(listener: TcpListener) -> Self {
        Self { listener }
    }

    /// Accepts one unauthenticated encrypted-frame connection.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] when socket acceptance or configuration fails.
    pub fn accept(&self) -> Result<EncryptedConnection, DirectTcpError> {
        let (stream, peer) = self.listener.accept().map_err(DirectTcpError::Accept)?;
        stream
            .set_nodelay(true)
            .map_err(DirectTcpError::Configure)?;
        // A non-blocking listener is used by the host accept loop, but each
        // accepted secure connection performs a blocking handshake in its own
        // worker. Explicitly restore blocking mode because some platforms
        // inherit the listener mode for accepted sockets.
        stream
            .set_nonblocking(false)
            .map_err(DirectTcpError::Configure)?;
        Ok(EncryptedConnection {
            stream,
            peer,
            decoder: EncryptedFrameDecoder::new(),
            pending_frames: VecDeque::new(),
        })
    }

    /// Returns the bound address and selected port.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] if the operating system cannot inspect the socket.
    pub fn local_addr(&self) -> Result<SocketAddr, DirectTcpError> {
        self.listener.local_addr().map_err(DirectTcpError::Inspect)
    }

    /// Configures whether [`accept`](Self::accept) should block when no client
    /// is ready. The host service uses non-blocking mode so UI commands can be
    /// handled while the listener remains alive.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError::Configure`] when the operating system rejects
    /// the requested mode.
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), DirectTcpError> {
        self.listener
            .set_nonblocking(nonblocking)
            .map_err(DirectTcpError::Configure)
    }
}

/// One TCP byte stream carrying only framed ciphertext records.
pub struct EncryptedConnection {
    stream: TcpStream,
    peer: SocketAddr,
    decoder: EncryptedFrameDecoder,
    pending_frames: VecDeque<Vec<u8>>,
}

/// Thread-safe cancellation handle for one TCP connection.
pub struct ConnectionControl {
    stream: TcpStream,
    peer: SocketAddr,
}

impl ConnectionControl {
    #[must_use]
    pub const fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    /// Immediately closes both directions of the underlying socket.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] when the operating system rejects shutdown.
    pub fn close(&self) -> Result<(), DirectTcpError> {
        self.stream
            .shutdown(Shutdown::Both)
            .map_err(DirectTcpError::Shutdown)
    }
}

impl EncryptedConnection {
    /// Connects to a direct Pix listener.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] for address resolution, connection, or socket
    /// configuration failures.
    pub fn connect(address: impl ToSocketAddrs) -> Result<Self, DirectTcpError> {
        let stream = TcpStream::connect(address).map_err(DirectTcpError::Connect)?;
        stream
            .set_nodelay(true)
            .map_err(DirectTcpError::Configure)?;
        let peer = stream.peer_addr().map_err(DirectTcpError::Inspect)?;
        Ok(Self {
            stream,
            peer,
            decoder: EncryptedFrameDecoder::new(),
            pending_frames: VecDeque::new(),
        })
    }

    #[must_use]
    pub const fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    /// Creates an independently owned handle that can revoke this connection.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] if the socket handle cannot be duplicated.
    pub fn control(&self) -> Result<ConnectionControl, DirectTcpError> {
        Ok(ConnectionControl {
            stream: self.stream.try_clone().map_err(DirectTcpError::Clone)?,
            peer: self.peer,
        })
    }

    /// Applies matching read and write deadlines to the socket.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] if the operating system rejects either timeout.
    pub fn set_timeout(&self, timeout: Option<Duration>) -> Result<(), DirectTcpError> {
        self.stream
            .set_read_timeout(timeout)
            .and_then(|()| self.stream.set_write_timeout(timeout))
            .map_err(DirectTcpError::Configure)
    }

    /// Applies a deadline only to reads, leaving encrypted event writes
    /// available for the normal socket lifetime.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError::Configure`] when the operating system rejects
    /// the requested timeout.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), DirectTcpError> {
        self.stream
            .set_read_timeout(timeout)
            .map_err(DirectTcpError::Configure)
    }

    /// Reads exactly one ciphertext record, retaining coalesced records for
    /// subsequent calls.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] for socket closure, I/O failure, or invalid
    /// framing.
    pub fn read_frame(&mut self) -> Result<Vec<u8>, DirectTcpError> {
        if let Some(frame) = self.pending_frames.pop_front() {
            return Ok(frame);
        }
        let frames = self.read_frames()?;
        let mut frames = VecDeque::from(frames);
        let Some(first) = frames.pop_front() else {
            return Err(DirectTcpError::EmptyRead);
        };
        self.pending_frames.extend(frames);
        Ok(first)
    }

    /// Reads until at least one complete ciphertext record is available.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] for socket closure, I/O failure, or an invalid
    /// frame prefix. No ciphertext is interpreted as an application message.
    pub fn read_frames(&mut self) -> Result<Vec<Vec<u8>>, DirectTcpError> {
        let mut chunk = [0_u8; 8192];
        loop {
            let count = self.stream.read(&mut chunk).map_err(DirectTcpError::Read)?;
            if count == 0 {
                return Err(DirectTcpError::Closed {
                    partial_frame: self.decoder.has_partial_frame(),
                });
            }
            let frames = self.decoder.push(&chunk[..count])?;
            if !frames.is_empty() {
                return Ok(frames);
            }
        }
    }

    /// Writes one already encrypted record with the v1 length prefix.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpError`] for invalid ciphertext size or socket failure.
    pub fn write_frame(&mut self, ciphertext: &[u8]) -> Result<(), DirectTcpError> {
        let frame = encode_encrypted_frame(ciphertext)?;
        self.stream.write_all(&frame).map_err(DirectTcpError::Write)
    }
}

#[derive(Debug, Error)]
pub enum DirectTcpError {
    #[error("failed to bind direct TCP listener: {0}")]
    Bind(io::Error),
    #[error("failed to accept direct TCP connection: {0}")]
    Accept(io::Error),
    #[error("failed to connect direct TCP transport: {0}")]
    Connect(io::Error),
    #[error("failed to configure direct TCP connection: {0}")]
    Configure(io::Error),
    #[error("failed to clone direct TCP connection handle: {0}")]
    Clone(io::Error),
    #[error("failed to shut down direct TCP connection: {0}")]
    Shutdown(io::Error),
    #[error("failed to inspect direct TCP listener: {0}")]
    Inspect(io::Error),
    #[error("failed to read encrypted TCP frame: {0}")]
    Read(io::Error),
    #[error("failed to write encrypted TCP frame: {0}")]
    Write(io::Error),
    #[error("peer closed encrypted TCP stream (partial_frame={partial_frame})")]
    Closed { partial_frame: bool },
    #[error("encrypted TCP read unexpectedly produced no complete frame")]
    EmptyRead,
    #[error(transparent)]
    Wire(#[from] WireError),
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
    use std::thread;

    use super::DirectTcpListener;

    #[test]
    fn loopback_transports_only_framed_ciphertext() {
        let socket = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .expect("bind loopback listener");
        let address = socket.local_addr().expect("listener address");
        let listener = DirectTcpListener::from_listener(socket);
        let server = thread::spawn(move || {
            let mut connection = listener.accept().expect("accept loopback client");
            let frames = connection.read_frames().expect("read ciphertext frame");
            assert_eq!(frames, vec![b"opaque-ciphertext".to_vec()]);
            connection
                .write_frame(b"opaque-response")
                .expect("write ciphertext frame");
        });
        let client = TcpStream::connect(address).expect("connect loopback client");
        let mut client = super::EncryptedConnection {
            peer: client.peer_addr().expect("peer address"),
            stream: client,
            decoder: pix_wire::EncryptedFrameDecoder::new(),
            pending_frames: std::collections::VecDeque::new(),
        };
        client
            .write_frame(b"opaque-ciphertext")
            .expect("write client frame");
        assert_eq!(
            client.read_frames().expect("read server frame"),
            vec![b"opaque-response".to_vec()]
        );
        server.join().expect("server thread");
    }
}
