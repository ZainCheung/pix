# Pi RPC coverage matrix

How Pi's RPC surface (verified against Pi 0.84.x, the supported line
`>=0.84.1, <0.85.0`) maps onto the Pix wire protocol. Update this file
whenever `SUPPORTED_PI_VERSION` or the bridge changes.

Status legend:

- **supported + exposed** — Pi capability mapped to a stable Pix wire surface.
- **supported + gated** — mapped, but only for clients that declare the
  matching capability (`protocol/schema/v1.md`).
- **supported + intentionally omitted** — deliberate product boundary.
- **TUI-only** — not available through Pi's RPC mode.

## Commands (host → Pi)

| Pi RPC command | Pix surface | Status |
| --- | --- | --- |
| `prompt` (with `images`, `streamingBehavior`) | `session.prompt` (+ `attachment.*` uploads) | supported + exposed / images gated by `attachments.v1` |
| `steer` (with `images`) | `session.steer` | supported + exposed / images gated |
| `follow_up` (with `images`) | `session.follow_up` | supported + exposed / images gated |
| `abort` | `session.abort` | supported + exposed |
| `get_state` | snapshots, `session.list` refresh | supported + exposed |
| `get_messages` | `session.snapshot.messages` | supported + exposed |
| `get_available_models` | `model.list` | supported + exposed |
| `set_model` | `model.set` | supported + exposed |
| `set_thinking_level` | `thinking.set` | supported + exposed |
| `compact` | `session.compact` | supported + exposed |
| `set_session_name` | `session.rename` | supported + exposed |
| `extension_ui_response` | `extension_ui.respond` | supported + exposed |
| `get_commands` | `session.snapshot.commands` | supported + gated (`commands.v1`) |
| `get_available_thinking_levels` | authoritative `thinking_levels` in snapshots | supported + gated (`thinking_levels.v1`) |
| `get_session_stats` | `session.snapshot.usage` | supported + gated (`usage.v1`) |
| `new_session` | `session.create` | supported + exposed (Pix owns the session ID) |
| `switch_session` | `session.attach` on a discovered session | supported + exposed |
| `cycle_model` / `cycle_thinking_level` | — | supported + intentionally omitted (typed `model.set` / `thinking.set` cover it; cycling is a TUI interaction) |
| `set_steering_mode` / `set_follow_up_mode` | — | supported + intentionally omitted (queue modes are not yet a product surface; revisit with queue UI) |
| `set_auto_compaction` / `set_auto_retry` / `abort_retry` | — | supported + intentionally omitted (retry/compaction policy stays Pi-side for now) |
| `bash` | — | supported + intentionally omitted (Pix is not a terminal; Pi's own tools cover execution) |
| `abort_bash` | — | supported + intentionally omitted (depends on `bash`) |
| `fork` / `clone` / `get_tree` / `get_fork_messages` | — | supported + intentionally omitted (session branching needs client UX first; `get_entries` cursor model also duplicates `get_messages`) |
| `get_entries` | — | supported + intentionally omitted (see `fork`) |
| `get_last_assistant_text` | — | supported + intentionally omitted (`message_end` events already deliver the text) |
| `export_html` | — | supported + intentionally omitted (writes files outside authorized workspace guarantees) |

## Events (Pi → host)

| Pi event | Pix event | Status |
| --- | --- | --- |
| `agent_start` | `session.state` (running) | supported + exposed |
| `agent_settled` | `session.state` (idle) | supported + exposed |
| `message_start` (user) | `user.message` | supported + exposed |
| `message_update` | `assistant.delta` | supported + exposed |
| `message_end` | `assistant.message` | supported + exposed |
| `tool_execution_start/update/end` | `tool.start/update/end` | supported + exposed |
| `extension_ui_request` | `extension_ui.request` | supported + exposed |
| `compaction_start/end` | `compaction` | supported + exposed |
| `queue_update` | `session.queue` + snapshot `queue` | supported + gated (`queue.v1`); cached in the runtime for reconnects |
| `agent_end` / `turn_start` / `turn_end` | — | supported + intentionally omitted (state events and message events already carry the semantics) |
| `auto_retry_start/end`, `summarization_retry_*` | — | supported + intentionally omitted (surfacing retries is a future snapshot field if needed) |
| `bash_execution_update` | — | supported + intentionally omitted (depends on `bash` command) |
| `extension_error` | `error` (`pi_unavailable`) | supported + exposed |
| `entry_appended` | — | supported + intentionally omitted (durable JSONL remains Pi's; Pix does not shadow it) |

## Deliberate non-goals

- Embedding the Pi Node SDK or running N AgentSessions in one daemon process:
  one Pi RPC child per active session stays the model (see
  `docs/ai-development/runtime-lifecycle-improvement-plan.md`).
- A second message database: Pi's native JSONL sessions stay the only durable
  conversation store.
- Exposing Pi `sourceInfo` paths, `sessionFile`, or any host filesystem path
  through the wire: the bridge strips them (`pi_bridge`).
- Terminal/TUI parity: terminal-only interactions remain outside the RPC
  boundary.
