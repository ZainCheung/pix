#!/bin/sh
set -eu

# Pix installer. This file is served by pix.deepoke.com/install.sh.
# It installs the CLI into ~/.local/bin and, on Apple Silicon, the Pix.app
# bundle into ~/Applications. It never needs root privileges.

repository="ZainCheung/pix"
api_url="https://api.github.com/repos/$repository/releases/latest"
release_page="https://github.com/$repository/releases/latest"
bin_dir=${PIX_INSTALL_DIR:-"$HOME/.local/bin"}

say() {
    printf '%s\n' "pix: $*"
}

fail() {
    printf '%s\n' "pix: $*" >&2
    exit 1
}

command -v curl >/dev/null 2>&1 || fail "curl is required. Install curl and run this command again."
command -v uname >/dev/null 2>&1 || fail "uname is required."

case "$(uname -s)" in
    Darwin)
        platform="macos"
        machine=$(uname -m)
        case "$machine" in
            arm64|aarch64) asset_suffix="macos-arm64" ;;
            *)
                say "Pix currently publishes a macOS Apple Silicon build."
                say "Open the latest release instead: $release_page"
                exit 1
                ;;
        esac
        ;;
    Linux)
        platform="linux"
        machine=$(uname -m)
        case "$machine" in
            x86_64|amd64) asset_suffix="x86_64-unknown-linux-gnu" ;;
            aarch64|arm64) asset_suffix="aarch64-unknown-linux-gnu" ;;
            *) fail "Unsupported Linux architecture: $machine" ;;
        esac
        ;;
    *)
        fail "Pix install.sh supports macOS and Linux. See $release_page for other options."
        ;;
esac

release_json=$(curl -fsSL --retry 2 --connect-timeout 8 "$api_url" 2>/dev/null || true)
tag=$(printf '%s\n' "$release_json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)

# The API is rate-limited independently from the release download endpoint.
# Resolve the same tag through GitHub's redirect before giving up.
if [ -z "$tag" ]; then
    latest_url=$(curl -fsSIL --retry 2 --connect-timeout 8 -o /dev/null -w '%{url_effective}' "$release_page" 2>/dev/null || true)
    case "$latest_url" in
        */releases/tag/*) tag=${latest_url##*/} ;;
    esac
fi

[ -n "$tag" ] || {
    say "The latest release does not have a readable tag yet."
    say "Open the latest release instead: $release_page"
    exit 1
}
version=${tag#v}

if [ "$platform" = "macos" ]; then
    asset="pix-$version-$asset_suffix.zip"
else
    asset="pix-$version-$asset_suffix.tar.gz"
fi

download_url="https://github.com/$repository/releases/download/$tag/$asset"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/pix-install.XXXXXX")
cleanup() {
    rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

archive="$tmp_dir/$asset"
say "Downloading Pix $version for $(uname -s) $(uname -m)"
if ! curl -fL --retry 2 --connect-timeout 8 -o "$archive" "$download_url"; then
    say "No matching release asset was found for this platform."
    say "Open the latest release instead: $release_page"
    exit 1
fi

mkdir -p "$bin_dir"

if [ "$platform" = "linux" ]; then
    tar -xzf "$archive" -C "$tmp_dir"
    extracted=$(find "$tmp_dir" -type f -path '*/bin/pix' -print | head -n 1)
    [ -n "$extracted" ] || fail "The release archive did not contain the Pix CLI."
    install -m 0755 "$extracted" "$bin_dir/pix" 2>/dev/null || cp "$extracted" "$bin_dir/pix"
    chmod 0755 "$bin_dir/pix"
else
    command -v unzip >/dev/null 2>&1 || fail "unzip is required to install the macOS app."
    unzip -q "$archive" -d "$tmp_dir/unpacked"
    app_path="$tmp_dir/unpacked/Pix.app"
    [ -d "$app_path" ] || fail "The release archive did not contain Pix.app."
    mkdir -p "$HOME/Applications"
    rm -rf "$HOME/Applications/Pix.app"
    cp -R "$app_path" "$HOME/Applications/Pix.app"
    [ -f "$app_path/Contents/Resources/pix" ] || fail "The macOS app did not contain the Pix CLI."
    install -m 0755 "$app_path/Contents/Resources/pix" "$bin_dir/pix" 2>/dev/null || cp "$app_path/Contents/Resources/pix" "$bin_dir/pix"
    chmod 0755 "$bin_dir/pix"
fi

say "Installed Pix $version to $bin_dir/pix"
if [ "$platform" = "macos" ]; then
    say "Installed Pix.app to $HOME/Applications/Pix.app"
fi

case ":${PATH:-}:" in
    *:"$bin_dir":*) ;;
    *)
        say "Add $bin_dir to PATH before running pix:"
        say "  export PATH=\"$bin_dir:\$PATH\""
        ;;
esac

say "Next step: pix setup"
