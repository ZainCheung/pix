# Repository boundary

`pix` is public source. The SwiftUI iOS and macOS clients are intentionally
private and are not mirrored here.

The compatibility boundary is versioned `pix-wire` plus
`protocol/schema/v1.md` and `protocol/fixtures/v1`. The private client pins a
Host tag and exact commit, builds the XCFramework from that checkout, and
embeds a CLI built from the same commit for macOS releases.

The public repository contains no Apple signing material, App Store metadata,
private workspace paths, or production Cloudflare credentials.
