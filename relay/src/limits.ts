// Shared relay limits. The frame limit mirrors pix-wire's 1 MiB ciphertext
// cap plus the 4-byte length prefix carried inside each binary message.

export const MAX_MESSAGE_BYTES = 1024 * 1024 + 4;

// Token-bucket rate limits per connection. Streaming assistant output sends
// many small frames, so the message budget is generous while still bounding
// abuse; the byte budget is what actually caps sustained throughput.
export const MESSAGE_BURST = 1024;
export const MESSAGES_PER_SECOND = 256;
export const BYTE_BURST = 16 * 1024 * 1024;
export const BYTES_PER_SECOND = 4 * 1024 * 1024;

// Channels with no connections for this long lose their pinned join proofs.
// Endpoints re-pin on the next join, so expiry only resets first-use binding.
export const IDLE_CHANNEL_TTL_MS = 24 * 60 * 60 * 1000;

// Close codes surfaced to endpoints. 4xxx codes are application-defined.
export const CLOSE_SUPERSEDED = 4008;
export const CLOSE_RATE_LIMITED = 4013;
export const CLOSE_MESSAGE_TOO_LARGE = 1009;
export const CLOSE_PROTOCOL_VIOLATION = 1008;
