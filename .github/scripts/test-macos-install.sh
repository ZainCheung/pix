#!/bin/sh
# shellcheck disable=SC2016
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
installer="$repository_root/website/public/install.sh"

case "$(uname -s)" in
    Darwin) ;;
    *)
        printf '%s\n' "macOS installer smoke test skipped outside macOS."
        exit 0
        ;;
esac

command -v ditto >/dev/null 2>&1 || {
    printf '%s\n' "ditto is required for the macOS installer smoke test." >&2
    exit 1
}

sh -n "$installer"
grep -F 'ditto -x -k "$archive" "$tmp_dir/unpacked"' "$installer" >/dev/null
grep -F 'ditto "$app_path" "$app_destination"' "$installer" >/dev/null

fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/pix-install-smoke.XXXXXX")
cleanup() {
    rm -rf "$fixture_root"
}
trap cleanup EXIT HUP INT TERM

fixture_app="$fixture_root/source/Pix.app"
fixture_cli="$fixture_app/Contents/Resources/pix"
mkdir -p "$(dirname "$fixture_cli")"
printf '%s\n' \
    '#!/bin/sh' \
    'printf "fixture-cli:%s\\n" "$*"' > "$fixture_cli"
chmod 0755 "$fixture_cli"
ditto -c -k --sequesterRsrc --keepParent "$fixture_app" \
    "$fixture_root/pix-test.zip"

fake_bin="$fixture_root/fake-bin"
mkdir -p "$fake_bin"
printf '%s\n' \
    '#!/bin/sh' \
    'case "${1:-}" in' \
    '    -s) printf "%s\\n" Darwin ;;' \
    '    -m) printf "%s\\n" arm64 ;;' \
    '    *) exit 1 ;;' \
    'esac' > "$fake_bin/uname"
chmod 0755 "$fake_bin/uname"
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'output=' \
    'expect_output=false' \
    'for arg in "$@"; do' \
    '    if [ "$expect_output" = true ]; then' \
    '        output=$arg' \
    '        expect_output=false' \
    '    elif [ "$arg" = "-o" ]; then' \
    '        expect_output=true' \
    '    fi' \
    'done' \
    'if [ -n "$output" ]; then' \
    '    cp "$PIX_INSTALL_FIXTURE_ARCHIVE" "$output"' \
    'else' \
    '    printf "%s\\n" '\''{"tag_name":"v0.1.99"}'\''' \
    'fi' > "$fake_bin/curl"
chmod 0755 "$fake_bin/curl"

test_path="$fake_bin:$PATH"
default_home="$fixture_root/default-home"
default_bin="$fixture_root/default-bin"

if [ ! -d "/Applications/Pix.app" ]; then
    HOME="$default_home" \
    PIX_INSTALL_DIR="$default_bin" \
    PIX_INSTALL_FIXTURE_ARCHIVE="$fixture_root/pix-test.zip" \
    PATH="$test_path" \
    sh "$installer"
    default_cli_output=$(HOME="$default_home" "$default_bin/pix" smoke)
    [ "$default_cli_output" = "fixture-cli:smoke" ]
    [ -x "$default_home/Applications/Pix.app/Contents/Resources/pix" ]
else
    printf '%s\n' "Default ~/Applications destination check skipped because /Applications/Pix.app already exists."
fi

custom_home="$fixture_root/custom-home"
custom_bin="$fixture_root/custom-bin"
custom_app="$fixture_root/custom user's/Pix.app"
HOME="$custom_home" \
PIX_APP_PATH="$custom_app" \
PIX_INSTALL_DIR="$custom_bin" \
PIX_INSTALL_FIXTURE_ARCHIVE="$fixture_root/pix-test.zip" \
PATH="$test_path" \
sh "$installer"
custom_cli_output=$(HOME="$custom_home" "$custom_bin/pix" custom)
[ "$custom_cli_output" = "fixture-cli:custom" ]
[ -x "$custom_app/Contents/Resources/pix" ]

override_app="$fixture_root/override/Pix.app"
ditto "$custom_app" "$override_app"
override_cli="$override_app/Contents/Resources/pix"
printf '%s\n' \
    '#!/bin/sh' \
    'printf "%s\\n" override-cli' > "$override_cli"
chmod 0755 "$override_cli"
override_output=$(HOME="$custom_home" PIX_APP_PATH="$override_app" "$custom_bin/pix")
[ "$override_output" = "override-cli" ]

# Exercise the generated shim's ambiguity guard without touching the real
# /Applications directory by substituting isolated paths in a test copy.
system_app="$fixture_root/system/Pix.app"
user_app="$fixture_root/user/Pix.app"
ditto "$custom_app" "$system_app"
ditto "$custom_app" "$user_app"
ambiguous_shim="$fixture_root/ambiguous-pix"
sed \
    -e "s|\$HOME/Applications/Pix.app|$user_app|g" \
    -e "s|/Applications/Pix.app|$system_app|g" \
    "$custom_bin/pix" > "$ambiguous_shim"
chmod 0755 "$ambiguous_shim"
ambiguity_error="$fixture_root/ambiguity-error"
if HOME="$fixture_root/ambiguous-home" "$ambiguous_shim" \
    > /dev/null 2> "$ambiguity_error"; then
    printf '%s\n' "the macOS shim did not reject two Pix.app bundles" >&2
    exit 1
fi
grep -F 'both /Applications and ~/Applications' "$ambiguity_error" >/dev/null

printf '%s\n' "macOS installer smoke test passed."
