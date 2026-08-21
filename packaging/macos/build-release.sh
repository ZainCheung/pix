#!/bin/sh
set -eu

# Build the public macOS menu-bar client and embed the matching pix CLI.
#
# The script is intentionally usable without Apple signing credentials. Set
# MACOS_CODE_SIGN_IDENTITY and MACOS_DEVELOPMENT_TEAM for a signed build. For
# notarization, either set MACOS_NOTARY_PROFILE for a local Keychain profile or
# set MACOS_NOTARY_KEY_PATH, MACOS_NOTARY_KEY_ID, and
# MACOS_NOTARY_ISSUER_ID for a Team API Key. Credentials never belong in this
# repository.

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
output_dir=${1:-"$repository_root/build/macos-release"}
version=$("$repository_root/scripts/version.sh")
case "$output_dir" in
    /*) ;;
    *) output_dir="$repository_root/$output_dir" ;;
esac

mac_arch=${MACOS_ARCH:-$(uname -m)}
case "$mac_arch" in
    arm64|aarch64)
        mac_arch=arm64
        default_cli_target=aarch64-apple-darwin
        ;;
    x86_64|amd64)
        mac_arch=x86_64
        default_cli_target=x86_64-apple-darwin
        ;;
    *)
        printf '%s\n' "Unsupported macOS architecture: $mac_arch" >&2
        exit 1
        ;;
esac
cli_target=${MACOS_CLI_TARGET:-$default_cli_target}

notary_profile=${MACOS_NOTARY_PROFILE:-}
notary_key_path=${MACOS_NOTARY_KEY_PATH:-}
notary_key_id=${MACOS_NOTARY_KEY_ID:-}
notary_issuer_id=${MACOS_NOTARY_ISSUER_ID:-}

if [ -n "$notary_profile" ] && {
    [ -n "$notary_key_path" ] ||
    [ -n "$notary_key_id" ] ||
    [ -n "$notary_issuer_id" ];
}; then
    printf '%s\n' "Choose MACOS_NOTARY_PROFILE or Team API Key credentials, not both." >&2
    exit 1
fi

if [ -n "$notary_key_path" ] || [ -n "$notary_key_id" ] || [ -n "$notary_issuer_id" ]; then
    [ -n "$notary_key_path" ] && [ -n "$notary_key_id" ] && [ -n "$notary_issuer_id" ] || {
        printf '%s\n' "MACOS_NOTARY_KEY_PATH, MACOS_NOTARY_KEY_ID, and MACOS_NOTARY_ISSUER_ID are required together." >&2
        exit 1
    }
    [ -f "$notary_key_path" ] || {
        printf '%s\n' "Notary API key file not found: $notary_key_path" >&2
        exit 1
    }
fi

if [ -n "$notary_profile" ] || [ -n "$notary_key_path" ]; then
    [ -n "${MACOS_CODE_SIGN_IDENTITY:-}" ] || {
        printf '%s\n' "Notarization requires MACOS_CODE_SIGN_IDENTITY." >&2
        exit 1
    }
fi

cargo build --manifest-path "$repository_root/Cargo.toml" -p pix-cli \
    --release --locked --target "$cli_target"
cli_binary="$repository_root/target/$cli_target/release/pix"
[ -x "$cli_binary" ] || {
    printf '%s\n' "Built Pix CLI is missing or not executable: $cli_binary" >&2
    exit 1
}

cd "$repository_root/apps/macos"
if command -v xcodegen >/dev/null 2>&1; then
    xcodegen generate
fi

mkdir -p "$output_dir"
archive_path="$output_dir/Pix.xcarchive"
rm -Rf "$archive_path"

if [ -n "${MACOS_CODE_SIGN_IDENTITY:-}" ]; then
    if [ -n "${MACOS_DEVELOPMENT_TEAM:-}" ]; then
        xcodebuild \
            -project Pix.xcodeproj \
            -scheme Pix \
            -configuration Release \
            -archivePath "$archive_path" \
            archive \
            ARCHS="$mac_arch" \
            ONLY_ACTIVE_ARCH=YES \
            CODE_SIGN_IDENTITY="$MACOS_CODE_SIGN_IDENTITY" \
            CODE_SIGN_STYLE=Manual \
            DEVELOPMENT_TEAM="$MACOS_DEVELOPMENT_TEAM"
    else
        xcodebuild \
            -project Pix.xcodeproj \
            -scheme Pix \
            -configuration Release \
            -archivePath "$archive_path" \
            archive \
            ARCHS="$mac_arch" \
            ONLY_ACTIVE_ARCH=YES \
            CODE_SIGN_IDENTITY="$MACOS_CODE_SIGN_IDENTITY" \
            CODE_SIGN_STYLE=Manual
    fi
else
    xcodebuild \
        -project Pix.xcodeproj \
        -scheme Pix \
        -configuration Release \
        -archivePath "$archive_path" \
        archive \
        ARCHS="$mac_arch" \
        ONLY_ACTIVE_ARCH=YES \
        CODE_SIGNING_ALLOWED=NO \
        CODE_SIGNING_REQUIRED=NO \
        CODE_SIGN_IDENTITY=
fi

app_path="$archive_path/Products/Applications/Pix.app"
resources_path="$app_path/Contents/Resources"
mkdir -p "$resources_path"
install -m 0755 "$cli_binary" "$resources_path/pix"

if [ -n "${MACOS_CODE_SIGN_IDENTITY:-}" ]; then
    # The CLI is a nested executable and must be signed before the outer app.
    codesign --force --options runtime --timestamp \
        --sign "$MACOS_CODE_SIGN_IDENTITY" "$resources_path/pix"
    codesign --force --options runtime --timestamp \
        --sign "$MACOS_CODE_SIGN_IDENTITY" "$app_path"
    codesign --verify --deep --strict --verbose=2 "$app_path"
fi

if [ -n "$notary_profile" ] || [ -n "$notary_key_path" ]; then
    notarization_zip="$output_dir/Pix-notarize.zip"
    rm -f "$notarization_zip"
    ditto -c -k --keepParent "$app_path" "$notarization_zip"
    if [ -n "$notary_profile" ]; then
        xcrun notarytool submit "$notarization_zip" \
            --keychain-profile "$notary_profile" \
            --wait
    else
        xcrun notarytool submit "$notarization_zip" \
            --key "$notary_key_path" \
            --key-id "$notary_key_id" \
            --issuer "$notary_issuer_id" \
            --wait
    fi
    xcrun stapler staple "$app_path"
    xcrun stapler validate "$app_path"
    spctl --assess --type execute --verbose=2 "$app_path"
fi

rm -Rf "$output_dir/Pix.app"
ditto "$app_path" "$output_dir/Pix.app"
ditto -c -k --sequesterRsrc --keepParent \
    "$output_dir/Pix.app" "$output_dir/pix-$version-macos-$mac_arch.zip"
printf '%s\n' "Wrote $output_dir/Pix.app"
