//! Writes canonical protocol v1 fixtures consumed by Rust and Swift tests.

use std::fs;
use std::path::{Path, PathBuf};

use pix_wire::{
    ClientEnvelope, ClientRequest, CommandScope, CommandSource, CommandSummary, CompactionEvent,
    ErrorCode, ExtensionUiAnswer, ExtensionUiRequest, HOST_CAPABILITIES, HostModelDefaults,
    HostSnapshot, HostSummary, MAX_ENCRYPTED_FRAME_BYTES, ModelSummary, PROTOCOL_MAJOR,
    RelayAccess, RelayRole, ServerEnvelope, ServerEvent, SessionQueue, SessionSnapshot,
    SessionState, SessionSummary, SessionUsage, ThinkingLevel, ToolEvent, WorkspaceAvailability,
    WorkspaceSummary, confirmation_code, encode_encrypted_frame, host_public_key_fingerprint,
    pairing_offer, relay_channel_id, relay_channel_secret_from_join_code, relay_join_proof,
};
use uuid::Uuid;

const WORKSPACE_ID: &str = "4cc891bc-30b9-4b5f-9298-38471d9b27ea";
const SESSION_ID: &str = "session-fixture";
const REQUEST_ID: u64 = 42;
const PAIRING_TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const RELAY_CHANNEL_SECRET: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const RELAY_URL: &str = "wss://relay.example.invalid";

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/fixtures/v1");
    fs::create_dir_all(&root).expect("create fixture directory");

    for (name, envelope) in client_fixtures() {
        write_bytes(&root, &name, &envelope.encode().expect("encode client"));
    }
    for (name, envelope) in server_fixtures() {
        write_bytes(&root, &name, &envelope.encode().expect("encode server"));
    }

    write_bytes(
        &root,
        "pairing-offer.json",
        &pairing_offer(PAIRING_TOKEN).expect("canonical pairing offer"),
    );
    write_raw(
        &root,
        "pairing-offer-invalid.json",
        &format!("{{ \"token\":\"{PAIRING_TOKEN}\"}}"),
    );
    write_raw(&root, "pairing-token-valid.txt", PAIRING_TOKEN);
    write_raw(&root, "pairing-token-invalid.txt", "not-a-token");
    write_raw(
        &root,
        "host-fingerprint.json",
        r#"{"public_key_hex":"0707070707070707070707070707070707070707070707070707070707070707","fingerprint":"8ddcd823c12c866d72b0df06ba9beba936ca6ba00292e0743b6aa6ba1a69fae7"}"#,
    );
    write_raw(
        &root,
        "confirmation-code.json",
        &format!(
            r#"{{"transcript_hex":"{}","code":"{}"}}"#,
            hex(&(0_u8..32).collect::<Vec<_>>()),
            confirmation_code(&(0_u8..32).collect::<Vec<_>>())
        ),
    );
    write_raw(
        &root,
        "pairing-expiry.json",
        r#"{"ttl_seconds":120,"single_use":true}"#,
    );
    write_raw(
        &root,
        "relay-channel.json",
        &format!(
            r#"{{"channel_secret":"{RELAY_CHANNEL_SECRET}","channel_id":"{}","host_join_proof":"{}","client_join_proof":"{}"}}"#,
            relay_channel_id(RELAY_CHANNEL_SECRET).expect("relay channel id"),
            relay_join_proof(RELAY_CHANNEL_SECRET, RelayRole::Host).expect("host proof"),
            relay_join_proof(RELAY_CHANNEL_SECRET, RelayRole::Client).expect("client proof"),
        ),
    );
    write_raw(
        &root,
        "relay-join-code.json",
        &format!(
            r#"{{"join_code":"AB10-1123","relay_url":"{RELAY_URL}","channel_secret":"{}"}}"#,
            relay_channel_secret_from_join_code("AB10-1123", RELAY_URL).expect("join secret"),
        ),
    );
    write_bytes(
        &root,
        "frame-valid.bin",
        &encode_encrypted_frame(b"ciphertext-fixture").expect("valid frame"),
    );
    write_bytes(
        &root,
        "frame-oversized.bin",
        &u32::try_from(MAX_ENCRYPTED_FRAME_BYTES + 1)
            .expect("prefix")
            .to_be_bytes(),
    );
    write_bytes(&root, "frame-empty.bin", &0_u32.to_be_bytes());
    write_raw(
        &root,
        "reject-protocol-unsupported.json",
        r#"{"protocol":2,"request_id":42,"type":"host.snapshot"}"#,
    );
    write_raw(
        &root,
        "reject-empty-session-id.json",
        r#"{"protocol":1,"request_id":42,"session_id":"","type":"session.attach"}"#,
    );

    assert_eq!(
        host_public_key_fingerprint(&[7_u8; 32]),
        "8ddcd823c12c866d72b0df06ba9beba936ca6ba00292e0743b6aa6ba1a69fae7"
    );
    println!("wrote fixtures to {}", root.display());
}

#[allow(clippy::too_many_lines)]
fn client_fixtures() -> Vec<(String, ClientEnvelope)> {
    let workspace_id = Uuid::parse_str(WORKSPACE_ID).expect("workspace id");
    vec![
        named(
            "client-host-snapshot.json",
            ClientRequest::HostSnapshot {
                capabilities: Vec::new(),
            },
        ),
        named(
            "client-host-snapshot-capabilities.json",
            ClientRequest::HostSnapshot {
                capabilities: vec![
                    "commands.v1".to_owned(),
                    "queue.v1".to_owned(),
                    "attachments.v1".to_owned(),
                    "usage.v1".to_owned(),
                    "thinking_levels.v1".to_owned(),
                    "session_metadata.v1".to_owned(),
                    "image_refs.v1".to_owned(),
                ],
            },
        ),
        named("client-host-defaults.json", ClientRequest::HostDefaults),
        named("client-workspace-list.json", ClientRequest::WorkspaceList),
        named(
            "client-session-list.json",
            ClientRequest::SessionList {
                workspace_id,
                limit: None,
            },
        ),
        named(
            "client-session-create.json",
            ClientRequest::SessionCreate {
                workspace_id,
                name: Some("Fixture session".to_owned()),
            },
        ),
        named(
            "client-session-attach.json",
            ClientRequest::SessionAttach {
                session_id: SESSION_ID.to_owned(),
            },
        ),
        named(
            "client-session-rename.json",
            ClientRequest::SessionRename {
                session_id: SESSION_ID.to_owned(),
                name: "Renamed fixture".to_owned(),
            },
        ),
        named(
            "client-session-release.json",
            ClientRequest::SessionRelease {
                session_id: SESSION_ID.to_owned(),
            },
        ),
        named(
            "client-session-prompt.json",
            ClientRequest::SessionPrompt {
                session_id: SESSION_ID.to_owned(),
                content: "Hello from Pix".to_owned(),
                attachments: Vec::new(),
            },
        ),
        named(
            "client-session-prompt-attachments.json",
            ClientRequest::SessionPrompt {
                session_id: SESSION_ID.to_owned(),
                content: "What does this screenshot show".to_owned(),
                attachments: vec!["attachment-fixture".to_owned()],
            },
        ),
        named(
            "client-session-steer.json",
            ClientRequest::SessionSteer {
                session_id: SESSION_ID.to_owned(),
                content: "Stop and try another path".to_owned(),
                attachments: Vec::new(),
            },
        ),
        named(
            "client-session-follow-up.json",
            ClientRequest::SessionFollowUp {
                session_id: SESSION_ID.to_owned(),
                content: "Also run the tests".to_owned(),
                attachments: Vec::new(),
            },
        ),
        named(
            "client-session-abort.json",
            ClientRequest::SessionAbort {
                session_id: SESSION_ID.to_owned(),
            },
        ),
        named(
            "client-session-compact.json",
            ClientRequest::SessionCompact {
                session_id: SESSION_ID.to_owned(),
                instructions: None,
            },
        ),
        named(
            "client-model-list.json",
            ClientRequest::ModelList {
                session_id: SESSION_ID.to_owned(),
            },
        ),
        named(
            "client-model-set.json",
            ClientRequest::ModelSet {
                session_id: SESSION_ID.to_owned(),
                provider: "fixture".to_owned(),
                model_id: "fixture-model".to_owned(),
            },
        ),
        named(
            "client-thinking-set.json",
            ClientRequest::ThinkingSet {
                session_id: SESSION_ID.to_owned(),
                level: ThinkingLevel::High,
            },
        ),
        named(
            "client-extension-ui-respond.json",
            ClientRequest::ExtensionUiRespond {
                session_id: SESSION_ID.to_owned(),
                extension_request_id: "ext-fixture".to_owned(),
                answer: ExtensionUiAnswer::Confirmed { confirmed: true },
            },
        ),
        named(
            "client-attachment-begin.json",
            ClientRequest::AttachmentBegin {
                session_id: SESSION_ID.to_owned(),
                attachment_id: "attachment-fixture".to_owned(),
                mime_type: "image/png".to_owned(),
                size: 4,
            },
        ),
        named(
            "client-attachment-chunk.json",
            ClientRequest::AttachmentChunk {
                attachment_id: "attachment-fixture".to_owned(),
                data: "aGVsbG8=".to_owned(),
            },
        ),
        named(
            "client-attachment-finish.json",
            ClientRequest::AttachmentFinish {
                attachment_id: "attachment-fixture".to_owned(),
            },
        ),
        named(
            "client-image-get.json",
            ClientRequest::ImageGet {
                session_id: SESSION_ID.to_owned(),
                image_ref:
                    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .to_owned(),
                offset: 0,
                limit: 1024,
            },
        ),
    ]
}

fn server_fixtures() -> Vec<(String, ServerEnvelope)> {
    let mut fixtures = host_and_session_events();
    fixtures.extend(stream_and_control_events());
    fixtures
}

fn fixture_workspace() -> (Uuid, WorkspaceSummary, ModelSummary, ToolEvent) {
    let workspace_id = Uuid::parse_str(WORKSPACE_ID).expect("workspace id");
    (
        workspace_id,
        WorkspaceSummary {
            id: workspace_id,
            name: "Fixture workspace".to_owned(),
            availability: WorkspaceAvailability::Available,
        },
        ModelSummary {
            provider: "fixture".to_owned(),
            id: "fixture-model".to_owned(),
            name: "Fixture Model".to_owned(),
            reasoning: true,
            input: Vec::new(),
            thinking_levels: Vec::new(),
        },
        ToolEvent {
            call_id: "call-fixture".to_owned(),
            name: "read".to_owned(),
            payload: serde_json::json!({"path":"README.md"}),
            is_error: None,
        },
    )
}

#[allow(clippy::too_many_lines)]
fn host_and_session_events() -> Vec<(String, ServerEnvelope)> {
    let (workspace_id, workspace, model, _) = fixture_workspace();
    vec![
        server("server-request-ack.json", None, ServerEvent::RequestAck),
        server(
            "server-host-snapshot.json",
            Some(REQUEST_ID),
            ServerEvent::HostSnapshot {
                snapshot: HostSnapshot {
                    host: HostSummary {
                        id: workspace_id,
                        display_name: "Fixture Mac".to_owned(),
                    },
                    workspaces: vec![workspace.clone()],
                    relay: None,
                    capabilities: HOST_CAPABILITIES
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect(),
                },
            },
        ),
        server(
            "server-host-snapshot-relay.json",
            Some(REQUEST_ID),
            ServerEvent::HostSnapshot {
                snapshot: HostSnapshot {
                    host: HostSummary {
                        id: workspace_id,
                        display_name: "Fixture Mac".to_owned(),
                    },
                    workspaces: vec![workspace.clone()],
                    relay: Some(RelayAccess {
                        url: RELAY_URL.to_owned(),
                        channel_secret: RELAY_CHANNEL_SECRET.to_owned(),
                    }),
                    capabilities: HOST_CAPABILITIES
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect(),
                },
            },
        ),
        server(
            "server-host-defaults.json",
            Some(REQUEST_ID),
            ServerEvent::HostDefaults {
                defaults: HostModelDefaults {
                    model: None,
                    models: Vec::new(),
                    thinking_level: None,
                },
            },
        ),
        server(
            "server-workspace-list.json",
            Some(REQUEST_ID),
            ServerEvent::WorkspaceList {
                workspaces: vec![workspace.clone()],
            },
        ),
        server(
            "server-workspace-changed.json",
            None,
            ServerEvent::WorkspaceChanged { workspace },
        ),
        server(
            "server-session-list.json",
            Some(REQUEST_ID),
            ServerEvent::SessionList {
                workspace_id,
                sessions: vec![SessionSummary {
                    id: SESSION_ID.to_owned(),
                    name: Some("Fixture session".to_owned()),
                    modified_at: "2026-08-13T00:00:00Z".to_owned(),
                    message_count: 1,
                    first_user_message: Some("Hello from Pix".to_owned()),
                    state: SessionState::Idle,
                }],
            },
        ),
        server(
            "server-session-snapshot.json",
            Some(REQUEST_ID),
            ServerEvent::SessionSnapshot {
                snapshot: SessionSnapshot {
                    id: SESSION_ID.to_owned(),
                    name: Some("Fixture session".to_owned()),
                    state: SessionState::Idle,
                    model: Some(model),
                    thinking_level: ThinkingLevel::Medium,
                    messages: vec![serde_json::json!({
                        "role": "user",
                        "content": "Hello from Pix"
                    })],
                    inflight_assistant: None,
                    through_sequence: None,
                    pending_prompts: Vec::new(),
                    active_tools: Vec::new(),
                    commands: Vec::new(),
                    queue: None,
                    usage: None,
                },
            },
        ),
        server(
            "server-session-snapshot-enriched.json",
            Some(REQUEST_ID),
            ServerEvent::SessionSnapshot {
                snapshot: SessionSnapshot {
                    id: SESSION_ID.to_owned(),
                    name: Some("Fixture session".to_owned()),
                    state: SessionState::Running,
                    model: Some(fixture_model_with_levels()),
                    thinking_level: ThinkingLevel::High,
                    messages: Vec::new(),
                    inflight_assistant: None,
                    through_sequence: None,
                    pending_prompts: vec![serde_json::json!({"status": "pending"})],
                    active_tools: Vec::new(),
                    commands: vec![
                        CommandSummary {
                            name: "review".to_owned(),
                            description: Some("Review current changes".to_owned()),
                            source: CommandSource::Extension,
                            scope: Some(CommandScope::User),
                        },
                        CommandSummary {
                            name: "fix-tests".to_owned(),
                            description: None,
                            source: CommandSource::Prompt,
                            scope: Some(CommandScope::Project),
                        },
                    ],
                    queue: Some(SessionQueue {
                        steering: vec!["Focus on error handling".to_owned()],
                        follow_up: vec!["Then run the tests".to_owned()],
                    }),
                    usage: Some(SessionUsage {
                        tokens_total: 16_512,
                        cost: 0.0125,
                        context_tokens: Some(4096),
                        context_window: Some(200_000),
                        context_percent: Some(2.05),
                    }),
                },
            },
        ),
        server(
            "server-session-state.json",
            None,
            ServerEvent::SessionState {
                session_id: SESSION_ID.to_owned(),
                state: SessionState::Running,
            },
        ),
        server(
            "server-session-queue.json",
            None,
            ServerEvent::SessionQueue {
                session_id: SESSION_ID.to_owned(),
                queue: SessionQueue {
                    steering: vec!["Focus on error handling".to_owned()],
                    follow_up: Vec::new(),
                },
            },
        ),
        server(
            "server-session-metadata.json",
            None,
            ServerEvent::SessionMetadata {
                session_id: SESSION_ID.to_owned(),
                commands: Some(Vec::new()),
                usage: None,
                thinking_levels: Some(vec![ThinkingLevel::Off, ThinkingLevel::High]),
            },
        ),
        server(
            "server-image-chunk.json",
            Some(REQUEST_ID),
            ServerEvent::ImageChunk {
                image_ref:
                    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .to_owned(),
                mime_type: "image/png".to_owned(),
                offset: 0,
                total_size: 2,
                eof: true,
                data: "aGk=".to_owned(),
            },
        ),
    ]
}

fn stream_and_control_events() -> Vec<(String, ServerEnvelope)> {
    let (_, _, model, tool) = fixture_workspace();
    vec![
        server(
            "server-user-message.json",
            None,
            ServerEvent::UserMessage {
                session_id: SESSION_ID.to_owned(),
                message: serde_json::json!({"role":"user","content":"Hello from Pix"}),
            },
        ),
        server(
            "server-assistant-delta.json",
            None,
            ServerEvent::AssistantDelta {
                session_id: SESSION_ID.to_owned(),
                delta: serde_json::json!({"type":"text_delta","delta":"Hi"}),
            },
        ),
        server(
            "server-assistant-message.json",
            None,
            ServerEvent::AssistantMessage {
                session_id: SESSION_ID.to_owned(),
                message: serde_json::json!({"role":"assistant","content":"Hi"}),
            },
        ),
        server(
            "server-tool-start.json",
            None,
            ServerEvent::ToolStart {
                session_id: SESSION_ID.to_owned(),
                tool: tool.clone(),
            },
        ),
        server(
            "server-tool-update.json",
            None,
            ServerEvent::ToolUpdate {
                session_id: SESSION_ID.to_owned(),
                tool: tool.clone(),
            },
        ),
        server(
            "server-tool-end.json",
            None,
            ServerEvent::ToolEnd {
                session_id: SESSION_ID.to_owned(),
                tool: ToolEvent {
                    is_error: Some(false),
                    payload: serde_json::json!({"content":"done"}),
                    ..tool
                },
            },
        ),
        server(
            "server-extension-ui-request.json",
            None,
            ServerEvent::ExtensionUiRequest {
                session_id: SESSION_ID.to_owned(),
                request: ExtensionUiRequest {
                    id: "ext-fixture".to_owned(),
                    method: "confirm".to_owned(),
                    payload: serde_json::json!({"text":"Allow write?"}),
                },
            },
        ),
        server(
            "server-compaction.json",
            None,
            ServerEvent::Compaction {
                session_id: SESSION_ID.to_owned(),
                compaction: CompactionEvent {
                    phase: "start".to_owned(),
                    reason: "context".to_owned(),
                    result: None,
                },
            },
        ),
        server(
            "server-model-list.json",
            Some(REQUEST_ID),
            ServerEvent::ModelList {
                session_id: SESSION_ID.to_owned(),
                models: vec![model],
            },
        ),
        server(
            "server-error.json",
            Some(REQUEST_ID),
            ServerEvent::Error {
                code: ErrorCode::NotFound,
                message: "Session not found".to_owned(),
                retryable: false,
            },
        ),
    ]
}

fn fixture_model_with_levels() -> ModelSummary {
    ModelSummary {
        provider: "fixture".to_owned(),
        id: "fixture-model".to_owned(),
        name: "Fixture Model".to_owned(),
        reasoning: true,
        input: Vec::new(),
        thinking_levels: vec![
            ThinkingLevel::Off,
            ThinkingLevel::Low,
            ThinkingLevel::High,
            ThinkingLevel::Xhigh,
        ],
    }
}

fn named(name: &str, request: ClientRequest) -> (String, ClientEnvelope) {
    (
        name.to_owned(),
        ClientEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id: REQUEST_ID,
            request,
        },
    )
}

fn server(name: &str, request_id: Option<u64>, event: ServerEvent) -> (String, ServerEnvelope) {
    (
        name.to_owned(),
        ServerEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id,
            event,
        },
    )
}

fn write_bytes(root: &Path, name: &str, bytes: &[u8]) {
    let mut payload = bytes.to_vec();
    if payload.last() != Some(&b'\n') {
        payload.push(b'\n');
    }
    fs::write(root.join(name), payload).expect("write fixture");
}

fn write_raw(root: &Path, name: &str, text: &str) {
    write_bytes(root, name, text.as_bytes());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("hex");
        output
    })
}
