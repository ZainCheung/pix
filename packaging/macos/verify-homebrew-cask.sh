#!/bin/sh
set -eu

# Validate a generated Pix Cask. Installation is opt-in because it downloads
# and moves an application into /Applications.
#
# Usage:
#   packaging/macos/verify-homebrew-cask.sh [path-to-cask]
#   PIX_HOMEBREW_CASK_INSTALL=1 packaging/macos/verify-homebrew-cask.sh

repository_root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
cask_path=${1:-"$repository_root/Casks/pix.rb"}

[ -f "$cask_path" ] || {
    printf '%s\n' "Homebrew Cask not found: $cask_path" >&2
    exit 1
}

if command -v brew >/dev/null 2>&1 && [ -n "${PIX_HOMEBREW_TAP:-}" ]; then
    # Homebrew style/audit resolve casks by tap name rather than arbitrary file
    # path. Set PIX_HOMEBREW_TAP when this file is checked from an actual tap
    # clone.
    cask_ref="$PIX_HOMEBREW_TAP/pix"
    HOMEBREW_NO_AUTO_UPDATE=1 brew style --cask "$cask_ref"
    # --new applies Homebrew/homebrew-cask's popularity gate. This repository
    # is validating its own first-party tap, so use the normal audit here;
    # official homebrew/cask submission can run --new separately.
    HOMEBREW_NO_AUTO_UPDATE=1 brew audit --cask --tap "$PIX_HOMEBREW_TAP" pix
elif [ -z "${PIX_HOMEBREW_TAP:-}" ]; then
    # The public pix checkout is also the source repository, not a Homebrew
    # tap clone. Keep local validation useful without mutating the developer's
    # Homebrew installation; the release workflow performs full tap checks.
    for required in \
        'cask "pix"' \
        'version "' \
        'sha256 "' \
        'url "https://' \
        'app "Pix.app"' \
        'binary "#{appdir}/Pix.app/Contents/Resources/pix"'; do
        grep -Fq "$required" "$cask_path" || {
            printf '%s\n' "Cask is missing required stanza: $required" >&2
            exit 1
        }
    done
    printf '%s\n' "Skipped Homebrew audit/style: $cask_path is not inside a tap clone."
else
    printf '%s\n' "brew is required when PIX_HOMEBREW_TAP is set" >&2
    exit 1
fi

if [ "${PIX_HOMEBREW_CASK_INSTALL:-0}" = "1" ]; then
    [ -n "${PIX_HOMEBREW_TAP:-}" ] || {
        printf '%s\n' "Set PIX_HOMEBREW_TAP to install a Cask from a tap clone" >&2
        exit 1
    }
    HOMEBREW_NO_AUTO_UPDATE=1 brew install --cask "$PIX_HOMEBREW_TAP/pix"
    installed_pix="$(brew --prefix)/bin/pix"
    [ -x "$installed_pix" ] || {
        printf '%s\n' "Pix CLI was not linked by the Cask: $installed_pix" >&2
        exit 1
    }
    HOMEBREW_NO_AUTO_UPDATE=1 brew uninstall --cask pix
fi

printf '%s\n' "Verified Pix Homebrew Cask: $cask_path"
