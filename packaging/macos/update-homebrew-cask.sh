#!/bin/sh
set -eu

# Render the first-party Pix Homebrew Cask for an immutable GitHub Release
# asset. The release workflow opens a PR with the generated file after the
# release is published; no tap or signing credentials are required here.
#
# Usage:
#   packaging/macos/update-homebrew-cask.sh VERSION URL SHA256 [OUTPUT]

repository_root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
version=${1:?usage: update-homebrew-cask.sh <version> <url> <sha256> [output]}
url=${2:?usage: update-homebrew-cask.sh <version> <url> <sha256> [output]}
sha256=${3:?usage: update-homebrew-cask.sh <version> <url> <sha256> [output]}
output=${4:-"$repository_root/Casks/pix.rb"}

case "$version" in
    ''|*[!0-9A-Za-z.+-]*)
        printf '%s\n' "Invalid Pix version: $version" >&2
        exit 1
        ;;
esac

case "$url" in
    https://*) ;;
    *)
        printf '%s\n' "Cask URL must use HTTPS: $url" >&2
        exit 1
        ;;
esac

if ! printf '%s' "$sha256" | grep -Eq '^[0-9a-fA-F]{64}$'; then
    printf '%s\n' "Cask SHA-256 must contain exactly 64 hexadecimal characters" >&2
    exit 1
fi

output_dir=$(dirname -- "$output")
mkdir -p "$output_dir"
temporary=$(mktemp "$output.tmp.XXXXXX")
trap 'rm -f "$temporary"' EXIT HUP INT TERM

cat > "$temporary" <<EOF
cask "pix" do
  version "$version"
  sha256 "$sha256"

  url "$url"
  name "Pix"
  desc "Secure menu-bar host for Pi"
  homepage "https://github.com/ZainCheung/pix"

  depends_on arch: :arm64
  depends_on macos: :sonoma

  app "Pix.app"
  # Expose the CLI embedded in Pix.app; this is a launcher for the same
  # canonical binary the menu-bar app uses, not a second CLI installation.
  binary "#{appdir}/Pix.app/Contents/Resources/pix", target: "pix"

  # Unload the user LaunchAgent and quit the menu-bar app, but preserve the
  # Host configuration, Keychain identity, authorized workspaces, and Pi
  # native session files.
  uninstall launchctl: "com.deepoke.pix.host",
            quit:      "com.pix.macos"
end
EOF

mv "$temporary" "$output"
trap - EXIT HUP INT TERM
printf '%s\n' "Wrote $output"
