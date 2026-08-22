#!/bin/sh
set -eu

# Classify a change set for the CI workflow. The script intentionally defaults
# to the safe side: an unknown path or an unavailable git base runs every
# existing check instead of silently reducing coverage.

: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
: "${GITHUB_EVENT_NAME:?GITHUB_EVENT_NAME is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"

all=false
rust=false
relay=false
apple=false
macos=false
linux=false
relay_deploy=false

mark_all() {
    all=true
}

write_outputs() {
    {
        printf 'all=%s\n' "$all"
        printf 'rust=%s\n' "$rust"
        printf 'relay=%s\n' "$relay"
        printf 'apple=%s\n' "$apple"
        printf 'macos=%s\n' "$macos"
        printf 'linux=%s\n' "$linux"
        printf 'relay_deploy=%s\n' "$relay_deploy"
    } >> "$GITHUB_OUTPUT"
}

case "$GITHUB_EVENT_NAME" in
    workflow_dispatch)
        mark_all
        write_outputs
        exit 0
        ;;
    pull_request)
        base_sha=${GITHUB_EVENT_PULL_REQUEST_BASE_SHA:-}
        ;;
    push)
        base_sha=${GITHUB_EVENT_BEFORE:-}
        ;;
    *)
        mark_all
        write_outputs
        exit 0
        ;;
esac

case "$base_sha" in
    ''|0000000000000000000000000000000000000000)
        mark_all
        write_outputs
        exit 0
        ;;
esac

if ! git cat-file -e "$base_sha^{commit}" 2>/dev/null; then
    # A full checkout should already contain this object. If GitHub changes
    # that behavior, fail open rather than skip a required validation job.
    mark_all
    write_outputs
    exit 0
fi

changed_paths=$(git diff --name-only "$base_sha" "$GITHUB_SHA")

while IFS= read -r path; do
    [ -n "$path" ] || continue

    case "$path" in
        .github/*|Cargo.toml|Cargo.lock)
            mark_all
            ;;
        protocol/*)
            mark_all
            ;;
        crates/pix-wire/*)
            rust=true
            apple=true
            linux=true
            ;;
        crates/pix-core/*|crates/pix-cli/*)
            rust=true
            linux=true
            ;;
        crates/*)
            mark_all
            ;;
        relay/*)
            relay=true
            relay_deploy=true
            ;;
        apps/macos/README.md|apps/macos/design*)
            ;;
        apps/macos/*)
            macos=true
            ;;
        packaging/apple/*)
            apple=true
            ;;
        packaging/linux/*)
            linux=true
            ;;
        packaging/macos/*|packaging/release/*|scripts/*)
            mark_all
            ;;
        docs/*|README.md|CONTRIBUTING.md|SECURITY.md|THIRD_PARTY_NOTICES.md|LICENSE|AGENTS.md|Casks/*)
            ;;
        *)
            # New source/configuration areas must not silently bypass CI.
            mark_all
            ;;
    esac
done <<EOF
$changed_paths
EOF

write_outputs
