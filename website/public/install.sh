#!/bin/sh
# shellcheck disable=SC2016
set -eu

# Pix installer. This file is served by pix.deepoke.com/install.sh.
# It installs the CLI into ~/.local/bin and, on Apple Silicon, the Pix.app
# bundle into an unambiguous Applications location. It never needs root
# privileges unless an existing /Applications/Pix.app is selected explicitly
# and is not writable by the current user.

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
    command -v ditto >/dev/null 2>&1 || fail "ditto is required to install the macOS app."
    mkdir -p "$tmp_dir/unpacked"
    ditto -x -k "$archive" "$tmp_dir/unpacked"
    app_path="$tmp_dir/unpacked/Pix.app"
    [ -d "$app_path" ] || fail "The release archive did not contain Pix.app."
    [ -x "$app_path/Contents/Resources/pix" ] || fail "The macOS app did not contain an executable Pix CLI."

    system_app_path="/Applications/Pix.app"
    user_app_path="$HOME/Applications/Pix.app"
    if [ -n "${PIX_APP_PATH:-}" ]; then
        app_destination=$PIX_APP_PATH
        case "$app_destination" in
            /Pix.app|/*/Pix.app) ;;
            *) fail "PIX_APP_PATH must be an absolute path ending in Pix.app." ;;
        esac
    elif [ -d "$system_app_path" ] && [ -d "$user_app_path" ]; then
        fail "Both $system_app_path and $user_app_path exist. Remove one or set PIX_APP_PATH explicitly."
    elif [ -d "$system_app_path" ]; then
        app_destination=$system_app_path
    else
        app_destination=$user_app_path
    fi

    mkdir -p "$(dirname "$app_destination")"
    rm -rf "$app_destination"
    ditto "$app_path" "$app_destination"
    [ -x "$app_destination/Contents/Resources/pix" ] || fail "The installed macOS app did not contain an executable Pix CLI."

    # Embed the destination selected during installation so a custom
    # PIX_APP_PATH remains the shim's default after the installer exits. The
    # runtime PIX_APP_PATH override still takes precedence for explicit use.
    shell_quote() {
        quoted=$(printf '%s' "$1" | sed "s/'/'\\\\''/g")
        printf "'%s'" "$quoted"
    }
    quoted_app_destination=$(shell_quote "$app_destination")

    # Keep the command as a tiny launcher for the canonical CLI inside
    # Pix.app. Sparkle replaces the whole app bundle in place, so a copied
    # executable would otherwise remain stale after a GUI update.
    shim="$tmp_dir/pix-shim"
    printf '%s\n' \
        '#!/bin/sh' \
        'set -eu' \
        'if [ -n "${PIX_APP_PATH:-}" ]; then' \
        '    if [ -x "${PIX_APP_PATH}/Contents/Resources/pix" ]; then' \
        '        exec "${PIX_APP_PATH}/Contents/Resources/pix" "$@"' \
        '    fi' \
        '    printf "%s\\n" "PIX_APP_PATH does not contain an executable Pix CLI: ${PIX_APP_PATH}" >&2' \
        '    exit 1' \
        'fi' \
        'if [ -d "/Applications/Pix.app" ] && [ -d "$HOME/Applications/Pix.app" ]; then' \
        '    printf "%s\\n" "Pix.app is installed in both /Applications and ~/Applications; set PIX_APP_PATH explicitly or remove one." >&2' \
        '    exit 1' \
        'fi' \
        "configured_app_path=$quoted_app_destination" \
        'if [ -x "$configured_app_path/Contents/Resources/pix" ]; then' \
        '    exec "$configured_app_path/Contents/Resources/pix" "$@"' \
        'fi' \
        'for app_path in "/Applications/Pix.app" "$HOME/Applications/Pix.app"; do' \
        '    if [ -x "$app_path/Contents/Resources/pix" ]; then' \
        '        exec "$app_path/Contents/Resources/pix" "$@"' \
        '    fi' \
        'done' \
        'printf "%s\\n" "Pix.app is not installed. Run https://pix.deepoke.com/install.sh again." >&2' \
        'exit 1' > "$shim"
    install -m 0755 "$shim" "$bin_dir/pix" 2>/dev/null || {
        cp "$shim" "$bin_dir/pix"
        chmod 0755 "$bin_dir/pix"
    }
fi

say "Installed Pix $version to $bin_dir/pix"
if [ "$platform" = "macos" ]; then
    say "Installed Pix.app to $app_destination"
fi

case ":${PATH:-}:" in
    *:"$bin_dir":*) ;;
    *)
        say "Add $bin_dir to PATH before running pix:"
        say "  export PATH=\"$bin_dir:\$PATH\""
        ;;
esac

say "Next step: pix setup"
