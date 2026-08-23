use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::time::{Duration, SystemTime};

use pix_core::{
    ApprovedDevice, ConfigStore, ConnectionRegistry, DirectTcpListener, EncryptedConnection,
    HostConfig, PairingCoordinator,
};
use tempfile::tempdir;

fn connected_pair() -> (EncryptedConnection, TcpStream) {
    let socket =
        TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).expect("bind loopback");
    let address = socket.local_addr().expect("listener address");
    let listener = DirectTcpListener::from_listener(socket);
    let client = TcpStream::connect(address).expect("connect loopback");
    let server = listener.accept().expect("accept loopback");
    (server, client)
}

#[test]
fn registry_closes_every_live_connection_for_revoked_device() {
    let registry = ConnectionRegistry::new();
    let device = ApprovedDevice {
        id: "device-1".to_owned(),
        name: "Phone".to_owned(),
        public_key: vec![1_u8; 32],
        paired_at: chrono::Utc::now(),
    };
    let (server_one, mut client_one) = connected_pair();
    let (server_two, mut client_two) = connected_pair();
    client_one
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("client one timeout");
    client_two
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("client two timeout");
    registry
        .register(&device, server_one.control().expect("first control"))
        .expect("register first");
    registry
        .register(&device, server_two.control().expect("second control"))
        .expect("register second");

    assert_eq!(registry.revoke_device(&device.id).expect("revoke"), 2);
    assert_eq!(registry.active_for_device(&device.id), 0);
    let mut buffer = [0_u8; 1];
    assert_eq!(
        std::io::Read::read(&mut client_one, &mut buffer).expect("first EOF"),
        0
    );
    assert_eq!(
        std::io::Read::read(&mut client_two, &mut buffer).expect("second EOF"),
        0
    );
    assert!(
        registry
            .register(&device, server_one.control().expect("new control"))
            .is_err()
    );
}

#[test]
fn persisted_revocation_and_disconnect_are_one_host_operation() {
    let directory = tempdir().expect("temporary config directory");
    let store = ConfigStore::new(directory.path().join("config.json"));
    store
        .save(&HostConfig::new("Revocation host"))
        .expect("initial config");
    let coordinator = PairingCoordinator::new(store.clone());
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(60_000);
    let offer = coordinator.issue_offer(now).expect("offer");
    let pending = coordinator
        .begin_approval(&offer.token, "Phone", &[8_u8; 32], &[9_u8; 32], now)
        .expect("pending approval");
    let device = coordinator.approve(pending.id, now).expect("approve");
    let registry = ConnectionRegistry::new();
    let (server, _client) = connected_pair();
    registry
        .register(&device, server.control().expect("control"))
        .expect("register connection");

    let revocation = coordinator
        .revoke_and_disconnect(&device.id, &registry)
        .expect("revoke and disconnect");
    assert_eq!(revocation.device.id, device.id);
    assert_eq!(revocation.closed_connections, 1);
    assert!(!revocation.connection_cleanup_failed);
    assert!(store.load().expect("config").devices.is_empty());
    assert!(coordinator.authenticate_peer(&[8_u8; 32]).is_err());
}
