#!/bin/sh
set -eu

# Builds a reproducible Linux tarball.
#
# Usage:
#   packaging/linux/build-release.sh <target-triple> [output-directory]
#
# SOURCE_DATE_EPOCH defaults to zero for local reproducibility. Release CI
# should set it to the chosen release/commit timestamp.

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
target=${1:?usage: build-release.sh <target-triple> [output-directory]}
output_dir=${2:-"$repository_root/target/release-pkg"}

case "$target" in
    x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;;
    *)
        printf '%s\n' "Unsupported Linux target: $target" >&2
        printf '%s\n' "Supported targets: x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu" >&2
        exit 1
        ;;
esac

source_date_epoch=${SOURCE_DATE_EPOCH:-0}
case "$source_date_epoch" in
    ''|*[!0-9]*)
        printf '%s\n' "SOURCE_DATE_EPOCH must be a non-negative integer" >&2
        exit 1
        ;;
esac
export SOURCE_DATE_EPOCH="$source_date_epoch"

cd "$repository_root"

rustup target list --installed | grep -qx "$target" || {
    printf '%s\n' "Missing Rust target: $target" >&2
    printf '%s\n' "Install it with: rustup target add $target" >&2
    exit 1
}

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
case "$version" in
    ''|*[!0-9A-Za-z.+-]*)
        printf '%s\n' "Invalid workspace version for a Linux package: $version" >&2
        exit 1
        ;;
esac

cargo build -p pix-cli --release --locked --target "$target"

mkdir -p "$output_dir"
staging="$output_dir/pix-$version-$target"
rm -Rf "$staging"
mkdir -p "$staging/bin" "$staging/share/doc/pix" "$staging/share/pix/systemd"

cp "target/$target/release/pix" "$staging/bin/pix"
cp README.md "$staging/share/doc/pix/README.md"
cp packaging/linux/pix.service "$staging/share/pix/systemd/pix.service"
chmod 0755 "$staging/bin/pix"
chmod 0644 "$staging/share/doc/pix/README.md" "$staging/share/pix/systemd/pix.service"

# Normalize all filesystem metadata before archiving. GNU tar also applies
# these values to directory entries and imposes a stable lexical order.
find "$staging" -exec touch -d "@$source_date_epoch" {} +
archive="$output_dir/pix-$version-$target.tar.gz"
tar_file="$output_dir/.pix-$version-$target.tar"
trap 'rm -f "$tar_file"' EXIT HUP INT TERM
rm -f "$archive"
rm -f "$tar_file"
tar --sort=name \
    --owner=0 --group=0 --numeric-owner \
    --mtime="@$source_date_epoch" \
    -C "$output_dir" -cf "$tar_file" "pix-$version-$target"
gzip -n -c "$tar_file" > "$archive"
[ -s "$archive" ] || {
    printf '%s\n' "Failed to create non-empty release archive: $archive" >&2
    exit 1
}

rm -Rf "$staging"
printf '%s\n' "Wrote $archive"
