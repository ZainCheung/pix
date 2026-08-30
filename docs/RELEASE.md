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
builds native Linux x86_64 and ARM64 artifacts, builds the Apple XCFramework,
and builds the macOS arm64 application on a GitHub-hosted `macos-15` runner.
The macOS job references the protected `apple-release` Environment: after
approval it imports the Developer ID certificate into a temporary Keychain,
signs the app and embedded CLI, notarizes with the Team API Key, staples the
ticket, verifies Gatekeeper readiness, and removes all signing material. The
workflow then generates the SBOM/license report and one `SHA256SUMS` manifest,
creates an artifact provenance attestation, and publishes a draft only after
all assets are ready. The release workflow refuses tags that are not contained
in `origin/main`.

The published files use stable names:

```text
pix-<version>-x86_64-unknown-linux-gnu.tar.gz
pix-<version>-aarch64-unknown-linux-gnu.tar.gz
pix_<version>_amd64.deb
pix_<version>_arm64.deb
pix-<version>-1.x86_64.rpm
pix-<version>-1.aarch64.rpm
pix-wire-<version>-apple.zip
pix-<version>-macos-arm64.dmg
pix-<version>-macos-arm64.zip
pix-<version>-sbom.spdx.json
pix-<version>-licenses.txt
SHA256SUMS
```

The Apple wire archive contains `PixWireFFI.xcframework`, `PixWire.swift`,
`VERSION`, and `COMMIT`. The macOS DMG and ZIP contain the same self-contained
`Pix.app` with a `pix` CLI built from the same source commit. The DMG includes
an `/Applications` shortcut for drag-and-drop installation and is submitted to
Apple notarization. The ZIP retains the stapled app for script and Homebrew
compatibility; CI validates the bundle after extraction. The Apple wire archive
is a static XCFramework artifact and does not participate in Developer ID
notarization.

## Homebrew Cask

After a published stable release, `.github/workflows/homebrew-cask.yml`
downloads the macOS asset, verifies the signed bundle and Gatekeeper result,
renders `Casks/pix.rb`, runs Homebrew Cask validation, and opens a pull request
against this repository. The Cask installs `Pix.app` and links the bundled
`pix` executable into Homebrew's `bin` directory. It never removes Pix Host
configuration, Keychain identity, authorized workspaces, or Pi session files.

The first-party Cask is generated only after the release asset passes the
Developer ID/notarization gate. The current release workflow publishes arm64
only; add an Intel or universal asset before broadening the Cask architecture
constraint. Because this source repository is not named `homebrew-pix`, the
initial first-party tap setup uses the explicit URL form:

```sh
brew tap ZainCheung/pix https://github.com/ZainCheung/pix.git
brew install --cask ZainCheung/pix/pix
```

Homebrew requires explicit trust for non-official taps. Installing the fully
qualified Cask trusts only `ZainCheung/pix/pix`; if the short name is preferred
after tapping, run `brew trust --cask ZainCheung/pix/pix` first. A future
dedicated `homebrew-pix` tap can provide the conventional tap name without
changing the release asset or Cask contents.

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

Relay changes on `main` are tested by the reusable Relay checks workflow and
deployed only after those checks pass. The standalone
`.github/workflows/relay-deploy.yml` workflow remains available for a manual
validated deployment. Configure the `relay-production` Environment with the
least-privileged `CLOUDFLARE_API_TOKEN` secret and the
`CLOUDFLARE_ACCOUNT_ID` variable. Deployment concurrency is serialized so two
production Worker updates cannot run at the same time.

## Website deployment

The public site is the `pix-website` Worker, deployed by Cloudflare Workers
Builds from `main`. The GitHub Actions `Website checks` workflow only typechecks
and builds when `website/**` changes. Workers Builds does not inherit that
filter: set Build watch paths on the Worker to include `website/*` and exclude
nothing, or a docs-only push to `main` will still deploy the site. The Worker
root directory (`website/`) is the build working directory, not the watch
filter. See [website/README.md](../website/README.md).

## Pi package release

The Pi TUI bridge is a separately versioned npm package:
`@zaincheung/pix`. Its source and manifest live entirely under
`packages/pix/`.

`.github/workflows/publish-pix-package.yml` validates package changes in pull
requests. It publishes only a push to `main` whose changed paths include
`packages/pix/**`; changes to Rust, the Host, the app, documentation, or the
workflow itself do not publish the npm package. The publish job also creates npm
provenance metadata.

Before enabling the workflow, configure the repository Actions secret
`NPM_TOKEN` with a token that can publish the public `@zaincheung` scope. npm
versions are immutable, so every package publication must bump the `version` in
`packages/pix/package.json`. A package-only change that reuses an already
published version intentionally fails the workflow instead of silently
publishing stale contents.
