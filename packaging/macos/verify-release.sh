#!/bin/sh
set -eu

# Verify a Pix macOS application bundle. Unsigned source builds can be checked
# for structure with MACOS_SKIP_CODESIGN=1; distribution builds should leave
# code-signature, Gatekeeper, and stapled-ticket checks enabled.

app_path=${1:?usage: verify-release.sh <path-to-Pix.app>}

[ -d "$app_path" ] || {
    printf '%s\n' "Application bundle not found: $app_path" >&2
    exit 1
}

cli_path="$app_path/Contents/Resources/pix"
[ -x "$cli_path" ] || {
    printf '%s\n' "Embedded Pix CLI is missing or not executable: $cli_path" >&2
    exit 1
}

sparkle_framework="$app_path/Contents/Frameworks/Sparkle.framework"
[ -d "$sparkle_framework" ] || {
    printf '%s\n' "Embedded Sparkle.framework is missing: $sparkle_framework" >&2
    exit 1
}

info_plist="$app_path/Contents/Info.plist"
[ -f "$info_plist" ] || {
    printf '%s\n' "Application Info.plist is missing: $info_plist" >&2
    exit 1
}

for key in SUFeedURL SUPublicEDKey; do
    value=$(plutil -extract "$key" raw -o - "$info_plist" 2>/dev/null || true)
    [ -n "$value" ] || {
        printf '%s\n' "Application Info.plist is missing $key" >&2
        exit 1
    }
done
automatic_updates=$(plutil -extract SUAllowsAutomaticUpdates raw -o - "$info_plist" 2>/dev/null || true)
[ "$automatic_updates" = "false" ] || {
    printf '%s\n' "Application Info.plist must disable automatic installation" >&2
    exit 1
}
automatic_checks=$(plutil -extract SUEnableAutomaticChecks raw -o - "$info_plist" 2>/dev/null || true)
[ "$automatic_checks" = "true" ] || {
    printf '%s\n' "Application Info.plist must enable automatic update checks" >&2
    exit 1
}

if [ "${MACOS_SKIP_CODESIGN:-0}" != "1" ]; then
    codesign --verify --deep --strict --verbose=2 "$app_path"
    if [ "${MACOS_SKIP_GATEKEEPER:-0}" != "1" ] && command -v spctl >/dev/null 2>&1; then
        spctl --assess --type execute --verbose=2 "$app_path"
    fi
    if command -v xcrun >/dev/null 2>&1; then
        xcrun stapler validate "$app_path"
    fi
fi

printf '%s\n' "Verified Pix macOS bundle: $app_path"
