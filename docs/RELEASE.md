# Release workflow

Pix has one product version source: `[workspace.package].version` in the root
`Cargo.toml`. Every workspace crate must use `version.workspace = true`, and a
release tag must match that value exactly:

```text
Cargo.toml: 0.1.0
Git tag:    v0.1.0
```

The wire protocol version and Relay deployment revision are independent of the
product version. A product release does not deploy the Relay.

## GitHub release

After the version change has landed on `main`:

```sh
git tag v0.1.0
git push origin v0.1.0
```

`.github/workflows/release.yml` validates the tag, verifies the workspace,
builds native Linux x86_64 and ARM64 artifacts, builds the public unsigned
macOS arm64 application archive, builds the Apple XCFramework, generates the
SBOM/license report and one `SHA256SUMS` manifest, creates an artifact
provenance attestation, and publishes a draft only after all assets are ready.
The release workflow refuses tags that are not contained in `origin/main`.

The published files use stable names:

```text
pix-<version>-x86_64-unknown-linux-gnu.tar.gz
pix-<version>-aarch64-unknown-linux-gnu.tar.gz
pix_<version>_amd64.deb
pix_<version>_arm64.deb
pix-<version>-1.x86_64.rpm
pix-<version>-1.aarch64.rpm
pix-wire-<version>-apple.zip
pix-<version>-macos-arm64.zip
pix-<version>-sbom.spdx.json
pix-<version>-licenses.txt
SHA256SUMS
```

The Apple wire archive contains `PixWireFFI.xcframework`, `PixWire.swift`,
`VERSION`, and `COMMIT`. The macOS archive contains a self-contained
`Pix.app` with a `pix` CLI built from the same source commit. The public
archive is unsigned; Developer ID signing and notarization are performed in a
protected release environment when distribution requires it.

## Local packaging

`scripts/version.sh` is the shared version reader used by packaging and CI.
For a local all-in-one Linux build:

```sh
SOURCE_DATE_EPOCH=0 packaging/linux/release.sh
```

For CI-style separation, run one target-specific build and package step per
runner, then finalize the collected directory once:

```sh
packaging/linux/build-release.sh x86_64-unknown-linux-gnu dist
packaging/linux/package.sh x86_64-unknown-linux-gnu dist
packaging/macos/build-release.sh dist
SOURCE_DATE_EPOCH=0 packaging/release/finalize.sh dist
```

## Relay deployment

Relay changes on `main` are tested and deployed only by
`.github/workflows/relay-deploy.yml`. Configure the `relay-production`
Environment with the least-privileged `CLOUDFLARE_API_TOKEN` secret and the
`CLOUDFLARE_ACCOUNT_ID` variable. Deployment concurrency is serialized so two
production Worker updates cannot run at the same time.
