#!/bin/sh
set -eu

# Local all-in-one Linux release helper. Platform jobs should call
# build-release.sh and package.sh independently, then run the target-agnostic
# packaging/release/finalize.sh once after collecting all artifacts.
#
# Usage:
#   packaging/linux/release.sh [target-triple ...]
#
# Defaults to x86_64-unknown-linux-gnu and aarch64-unknown-linux-gnu.

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
output_dir=${PIX_RELEASE_DIR:-"$repository_root/target/release-pkg"}

if [ "$#" -eq 0 ]; then
    set -- x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
fi

for target in "$@"; do
    case "$target" in
        x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;;
        *)
            printf '%s\n' "Unsupported Linux target: $target" >&2
            exit 1
            ;;
    esac
done

source_date_epoch=${SOURCE_DATE_EPOCH:-0}
case "$source_date_epoch" in
    ''|*[!0-9]*)
        printf '%s\n' "SOURCE_DATE_EPOCH must be a non-negative integer" >&2
        exit 1
        ;;
esac
export SOURCE_DATE_EPOCH="$source_date_epoch"

cd "$repository_root"
mkdir -p "$output_dir"

for target in "$@"; do
    packaging/linux/build-release.sh "$target" "$output_dir"
    PIX_PACKAGE_DEB=${PIX_PACKAGE_DEB:-1} \
        PIX_PACKAGE_RPM=${PIX_PACKAGE_RPM:-1} \
        packaging/linux/package.sh "$target" "$output_dir"
done

PIX_RELEASE_VERSION=$("$repository_root/scripts/version.sh") \
    packaging/release/finalize.sh "$output_dir"
