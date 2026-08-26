#!/bin/sh
set -eu

# Build the Debug menu-bar app, replace the loaded per-user host service with
# the matching embedded CLI, and launch the app. This is intentionally a
# development helper: release builds should use packaging/macos/build-release.sh.

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
derived_data=${PIX_MACOS_DERIVED_DATA:-"$repository_root/build/macos-debug"}
case "$derived_data" in
    /*) ;;
    *) derived_data="$repository_root/$derived_data" ;;
esac

if command -v xcodegen >/dev/null 2>&1; then
    (
        cd "$repository_root/apps/macos"
        xcodegen generate
    )
fi

xcodebuild \
    -project "$repository_root/apps/macos/Pix.xcodeproj" \
    -scheme Pix \
    -configuration Debug \
    -destination 'platform=macOS' \
    -derivedDataPath "$derived_data" \
    CODE_SIGNING_ALLOWED=NO \
    CODE_SIGNING_REQUIRED=NO \
    build

app_path="$derived_data/Build/Products/Debug/Pix.app"
cli_path="$app_path/Contents/Resources/pix"
test -x "$cli_path"

# Write the new LaunchAgent definition without starting it, then restart
# explicitly. The second step matters when --no-start left an older process
# loaded from a previous DerivedData directory.
"$cli_path" service install --adopt --no-start
"$cli_path" service restart

if [ "${PIX_MACOS_DEV_NO_OPEN:-0}" != "1" ]; then
    open "$app_path"
fi

printf '%s\n' "Pix Debug app: $app_path"
printf '%s\n' "Pix Debug CLI: $cli_path"
"$cli_path" service status
