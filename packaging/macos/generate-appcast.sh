#!/bin/sh
set -eu

# Generate Sparkle metadata for a notarized Pix release archive. The script
# deliberately delegates XML construction and EdDSA signing to Sparkle's
# generate_appcast tool; it never writes a private key to disk or places it in
# a process argument.
#
# Usage:
#   SPARKLE_PRIVATE_KEY=... \
#     packaging/macos/generate-appcast.sh <version> <tag> <release-directory>
#
# The same values can be supplied with VERSION, TAG, and RELEASE_DIR. Set
# SPARKLE_GENERATE_APPCAST to an explicit tool path (or SPARKLE_HOME to a
# Sparkle distribution root) when the tool is not on PATH.

if [ "$#" -eq 3 ]; then
    VERSION=$1
    TAG=$2
    RELEASE_DIR=$3
elif [ "$#" -eq 0 ]; then
    VERSION=${VERSION:-}
    TAG=${TAG:-}
    RELEASE_DIR=${RELEASE_DIR:-}
else
    printf '%s\n' "usage: generate-appcast.sh <version> <tag> <release-directory>" >&2
    exit 64
fi

[ -n "$VERSION" ] || {
    printf '%s\n' "VERSION is required" >&2
    exit 64
}
case "$VERSION" in
    *[-+]*)
        printf '%s\n' "Sparkle appcast generation currently supports stable releases only: $VERSION" >&2
        exit 64
        ;;
esac
[ -n "$TAG" ] || TAG="v$VERSION"
[ "$TAG" = "v$VERSION" ] || {
    printf '%s\n' "TAG must match the stable workspace version (v$VERSION): $TAG" >&2
    exit 64
}
[ -n "$RELEASE_DIR" ] || {
    printf '%s\n' "RELEASE_DIR is required" >&2
    exit 64
}
[ -n "${SPARKLE_PRIVATE_KEY:-}" ] || {
    printf '%s\n' "SPARKLE_PRIVATE_KEY is required to sign the appcast" >&2
    exit 64
}

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
case "$RELEASE_DIR" in
    /*) ;;
    *) RELEASE_DIR="$repository_root/$RELEASE_DIR" ;;
esac

archive_name="pix-$VERSION-macos-arm64.zip"
archive_path="$RELEASE_DIR/$archive_name"
[ -f "$archive_path" ] || {
    printf '%s\n' "macOS Sparkle archive not found: $archive_path" >&2
    exit 1
}

generate_appcast=${SPARKLE_GENERATE_APPCAST:-}
if [ -z "$generate_appcast" ] && [ -n "${SPARKLE_HOME:-}" ]; then
    generate_appcast="$SPARKLE_HOME/bin/generate_appcast"
fi
if [ -z "$generate_appcast" ]; then
    generate_appcast=$(command -v generate_appcast || true)
fi
[ -n "$generate_appcast" ] && [ -x "$generate_appcast" ] || {
    printf '%s\n' "Sparkle generate_appcast tool was not found" >&2
    printf '%s\n' "Set SPARKLE_GENERATE_APPCAST or SPARKLE_HOME." >&2
    exit 1
}

build_version=$(
    "$repository_root/scripts/macos-build-version.sh" "$VERSION"
)
download_url_prefix="https://github.com/ZainCheung/pix/releases/download/$TAG/"
release_notes_url="https://github.com/ZainCheung/pix/releases/tag/$TAG"
staging_dir=$(mktemp -d "${TMPDIR:-/tmp}/pix-appcast.XXXXXX")
cleanup() {
    rm -rf "$staging_dir"
}
trap cleanup EXIT HUP INT TERM

cp "$archive_path" "$staging_dir/$archive_name"

# Re-use the currently published feed when available so every release keeps
# its historical entries. A local appcast in the release directory takes
# precedence, which makes retries deterministic and keeps the script usable
# without network access.
appcast_url=${APPCAST_URL-https://github.com/ZainCheung/pix/releases/latest/download/appcast.xml}
if [ -f "$RELEASE_DIR/appcast.xml" ]; then
    cp "$RELEASE_DIR/appcast.xml" "$staging_dir/appcast.xml"
elif [ -n "$appcast_url" ]; then
    if curl -fsSL --retry 2 --connect-timeout 8 \
        "$appcast_url" \
        -o "$staging_dir/appcast.xml" 2>/dev/null; then
        # A release feed may be unavailable before its first appcast asset is
        # published. Treat any non-RSS response as an empty history instead of
        # passing it to generate_appcast, which would fail with an opaque XML
        # error.
        if ! python3 - "$staging_dir/appcast.xml" <<'PY'
import sys
import xml.etree.ElementTree as ET

try:
    root = ET.parse(sys.argv[1]).getroot()
except (ET.ParseError, OSError):
    raise SystemExit(1)
local_name = root.tag.rsplit("}", 1)[-1]
channel = next(
    (child for child in root if child.tag.rsplit("}", 1)[-1] == "channel"),
    None,
)
if local_name != "rss" or channel is None:
    raise SystemExit(1)
PY
        then
            rm -f "$staging_dir/appcast.xml"
        fi
    fi
fi

printf '%s' "$SPARKLE_PRIVATE_KEY" |
    "$generate_appcast" \
        --ed-key-file - \
        --maximum-versions 0 \
        --maximum-deltas 0 \
        --download-url-prefix "$download_url_prefix" \
        --full-release-notes-url "$release_notes_url" \
        --link "https://pix.deepoke.com" \
        "$staging_dir"

[ -s "$staging_dir/appcast.xml" ] || {
    printf '%s\n' "Sparkle did not produce appcast.xml" >&2
    exit 1
}

python3 - "$staging_dir/appcast.xml" "$archive_name" "$VERSION" "$build_version" "$download_url_prefix" <<'PY'
import sys
import urllib.parse
import xml.etree.ElementTree as ET

appcast_path, archive_name, short_version, build_version, url_prefix = sys.argv[1:]
sparkle_ns = "http://www.andymatuschak.org/xml-namespaces/sparkle"
root = ET.parse(appcast_path).getroot()
items = root.findall(".//item")
expected_url = urllib.parse.urljoin(url_prefix, archive_name)
matching_item = None
for item in items:
    enclosure = item.find("enclosure")
    if enclosure is None:
        continue
    if enclosure.get("url") == expected_url:
        matching_item = (item, enclosure)
        break

if matching_item is None:
    raise SystemExit(f"appcast has no enclosure for {expected_url}")

item, enclosure = matching_item
version_node = item.find(f"{{{sparkle_ns}}}version")
version = version_node.text if version_node is not None else None
if version != build_version:
    raise SystemExit(
        f"appcast sparkle:version {version!r} does not match {build_version!r}"
    )
short_version_node = item.find(f"{{{sparkle_ns}}}shortVersionString")
short_version_value = (
    short_version_node.text if short_version_node is not None else None
)
if short_version_value != short_version:
    raise SystemExit(
        "appcast sparkle:shortVersionString "
        f"{short_version_value!r} does not match {short_version!r}"
    )
signature = enclosure.get(f"{{{sparkle_ns}}}edSignature")
if not signature:
    raise SystemExit("appcast enclosure is missing sparkle:edSignature")
length = enclosure.get("length")
if not length or not length.isdigit() or int(length) <= 0:
    raise SystemExit("appcast enclosure has no valid archive length")
PY

cp "$staging_dir/appcast.xml" "$RELEASE_DIR/appcast.xml"
printf '%s\n' "Wrote $RELEASE_DIR/appcast.xml"
