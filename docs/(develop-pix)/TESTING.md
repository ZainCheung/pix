---
title: Testing
description: Choose the Pix test suite that protects the component you changed.
---

Run the smallest relevant suite while iterating, then run the full checks before
opening a pull request. Pix's CI uses the same commands.

## Fast checks

From the repository root:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

These checks cover the Rust workspace. Tests that need a real Pi are ignored
unless a compatible executable is available; `pix status` reports the Pi
version and startup options that the host detects.

## Test by changed area

| Changed area | Run |
| --- | --- |
| `crates/pix-core/` | `cargo test -p pix-core` or a focused test name such as `cargo test -p pix-core pairing` |
| `crates/pix-cli/` | `cargo test -p pix-cli --all-features --locked`; use a named integration test such as `cargo test -p pix-cli --test e2e_lan` when the environment supports it |
| `crates/pix-wire/` | `cargo test -p pix-wire`, then follow the [fixture workflow](#wire-protocol-and-fixtures) |
| `relay/` | `(cd relay && npm ci && npm test && npm run typecheck)` |
| `apps/macos/` | `xcodebuild test -project Pix.xcodeproj -scheme Pix -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO` from `apps/macos/` |
| `packages/pix/` | `npm pack --ignore-scripts --dry-run --json` from `packages/pix/`; the package workflow also validates its manifest and required files |
| `docs/` or `website/` | `(cd website && npm ci && npm run check-doc-links && npm run generate-routes && npm run typecheck && npm run build)` |
| `.github/scripts/detect-ci-changes.sh` | `sh .github/scripts/test-detect-ci-changes.sh` |

The macOS command runs on a Mac. Relay tests use the lockfile and the
Cloudflare Workers test pool; they do not deploy a Worker.

## Wire protocol and fixtures

The Rust fixture test reads `protocol/fixtures/v1/`, checks canonical encoding
for every request and event, and exercises invalid protocol, pairing, relay,
and frame-limit cases. The relay test suite reads the relay-channel fixture.

After changing a serializer, derivation, or versioned schema, regenerate from
the Rust implementation and inspect the complete diff:

```bash
cargo run -p pix-wire --example generate_fixtures
cargo test -p pix-wire
```

The [Wire protocol](/docs/wire-protocol) page explains which facts belong in
the schema and which fixtures consume them. A docs-only change does not require
fixture regeneration.

## Website and docs validation

The website check sequence is:

```bash
cd website
npm ci
npm run check-doc-links
npm run generate-routes
npm run typecheck
npm run build
```

`check-doc-links` checks source/navigation parity, internal `/docs` links,
repository-relative Markdown links, detectable anchors, loaded source files,
and route-group leakage in generated public URLs. Route generation must leave
`src/routeTree.gen.ts` unchanged in a committed checkout; the CI workflow checks
this with `git diff --exit-code`.

## CI path classification

`.github/scripts/detect-ci-changes.sh` classifies changed paths so unrelated
Rust, relay, Apple, macOS, and packaging jobs can be skipped. Unknown source
areas and missing git bases fail open to the full check set. The companion
`.github/scripts/test-detect-ci-changes.sh` creates a temporary repository and
asserts the classifications for representative paths.

Run the companion test after changing either script:

```bash
sh .github/scripts/test-detect-ci-changes.sh
```

## Release-related checks

Release verification reruns formatting, the full Rust suite, Clippy, and the
relay tests before packaging. Linux archive smoke tests, Apple wire builds,
macOS app signing, and artifact publication are described in the
[Release workflow](/docs/release). Check [Platform support](/docs/platform-support)
when a new target or artifact is proposed.

Before submitting a change, compare the affected component's tests with
[Architecture](/docs/architecture) and [Repository boundary](/docs/repository)
so the test selection matches the ownership boundary.
