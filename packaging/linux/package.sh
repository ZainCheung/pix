#!/bin/sh
set -eu

# Creates deb/rpm artifacts from a built Linux release archive.
#
# Usage:
#   packaging/linux/package.sh <target-triple> [output-directory]

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
target=${1:?usage: package.sh <target-triple> [output-directory]}
output_dir=${2:-"$repository_root/target/release-pkg"}

case "$target" in
    x86_64-unknown-linux-gnu) deb_arch=amd64; rpm_arch=x86_64 ;;
    aarch64-unknown-linux-gnu) deb_arch=arm64; rpm_arch=aarch64 ;;
    *)
        printf '%s\n' "Unsupported Linux target: $target" >&2
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

version=$("$repository_root/scripts/version.sh")
case "$version" in
    ''|*[!0-9A-Za-z.+-]*)
        printf '%s\n' "Invalid workspace version for a Linux package: $version" >&2
        exit 1
        ;;
esac

archive="$output_dir/pix-$version-$target.tar.gz"
[ -f "$archive" ] || {
    printf '%s\n' "Release archive not found: $archive" >&2
    printf '%s\n' "Run packaging/linux/build-release.sh $target first." >&2
    exit 1
}

staging="$output_dir/pkg-$version-$target"
rm -Rf "$staging"
mkdir -p "$staging"
tar -C "$staging" -xzf "$archive"
package_root="$staging/pix-$version-$target"
[ -x "$package_root/bin/pix" ] || {
    printf '%s\n' "Release archive does not contain an executable Pix CLI" >&2
    exit 1
}
[ -f "$package_root/share/doc/pix/README.md" ] || {
    printf '%s\n' "Release archive does not contain the Pix documentation" >&2
    exit 1
}
[ -f "$package_root/share/pix/systemd/pix.service" ] || {
    printf '%s\n' "Release archive does not contain the Pix systemd unit" >&2
    exit 1
}
command -v readelf >/dev/null 2>&1 || {
    printf '%s\n' "readelf is required to inspect the packaged CLI" >&2
    exit 1
}
machine=$(readelf -h "$package_root/bin/pix" | sed -n 's/^ *Machine: *//p')
case "$target:$machine" in
    x86_64-unknown-linux-gnu:*X86-64*|aarch64-unknown-linux-gnu:*AArch64*) ;;
    *)
        printf '%s\n' "Release archive CLI architecture does not match target $target" >&2
        exit 1
        ;;
esac
grep -a -F -q "$version" "$package_root/bin/pix" || {
    printf '%s\n' "Release archive CLI version does not contain package version $version" >&2
    exit 1
}

if [ "${PIX_PACKAGE_DEB:-1}" = "1" ]; then
    if command -v dpkg-deb >/dev/null 2>&1; then
        deb_root="$staging/deb"
        deb_dir="$deb_root/pix_${version}_${deb_arch}"
        rm -Rf "$deb_root"
        mkdir -p \
            "$deb_dir/DEBIAN" \
            "$deb_dir/usr/bin" \
            "$deb_dir/usr/share/doc/pix" \
            "$deb_dir/usr/lib/systemd/user"
        cat > "$deb_dir/DEBIAN/control" <<EOF
Package: pix
Version: $version
Section: utils
Priority: optional
Architecture: $deb_arch
Maintainer: Pix maintainers
Description: Lightweight remote interface for the Pi coding agent
 Pix runs Pi on the user's computer and presents authorized workspaces
 through the native Pix iOS app over LAN or an end-to-end encrypted relay.
EOF
        cp "$package_root/bin/pix" "$deb_dir/usr/bin/pix"
        cp "$package_root/share/doc/pix/README.md" "$deb_dir/usr/share/doc/pix/README.md"
        cp "$package_root/share/pix/systemd/pix.service" \
            "$deb_dir/usr/lib/systemd/user/pix.service"
        chmod 0755 "$deb_dir/usr/bin/pix"
        chmod 0644 \
            "$deb_dir/DEBIAN/control" \
            "$deb_dir/usr/share/doc/pix/README.md" \
            "$deb_dir/usr/lib/systemd/user/pix.service"
        find "$deb_dir" -exec touch -d "@$source_date_epoch" {} +
        deb_file="$output_dir/pix_${version}_${deb_arch}.deb"
        rm -f "$deb_file"
        dpkg-deb --build --root-owner-group "$deb_dir" "$deb_file"
        [ -s "$deb_file" ] || {
            printf '%s\n' "dpkg-deb produced an empty artifact" >&2
            exit 1
        }
        printf '%s\n' "Wrote $deb_file"
    else
        printf '%s\n' "dpkg-deb is unavailable; skipping deb artifact."
    fi
fi

if [ "${PIX_PACKAGE_RPM:-1}" = "1" ]; then
    if command -v rpmbuild >/dev/null 2>&1; then
        rpm_host_arch=$(rpm --eval '%{_arch}' 2>/dev/null || printf '%s' unknown)
        if [ "$rpm_host_arch" = "$rpm_arch" ]; then
            rpm_root="$staging/rpm"
        rm -Rf "$rpm_root"
        mkdir -p \
            "$rpm_root/BUILD" \
            "$rpm_root/RPMS" \
            "$rpm_root/SOURCES" \
            "$rpm_root/SPECS" \
            "$rpm_root/SRPMS" \
            "$rpm_root/RPMDB"
        release_date=$(date -u -d "@$source_date_epoch" '+%a %b %d %Y')
        cat > "$rpm_root/SPECS/pix.spec" <<EOF
%global debug_package %{nil}
Name:           pix
Version:        $version
Release:        1
Summary:        Lightweight remote interface for the Pi coding agent
License:        MIT
Source0:        pix-$version-$target.tar.gz
BuildArch:      $rpm_arch

%description
Pix runs Pi on the user's computer and presents only explicitly authorized
workspaces through the native Pix iOS app over LAN or an end-to-end encrypted
relay.

%prep
%setup -q -n pix-$version-$target

%build

%install
install -Dm0755 bin/pix %{buildroot}/usr/bin/pix
install -Dm0644 share/doc/pix/README.md %{buildroot}/usr/share/doc/pix/README.md
install -Dm0644 share/pix/systemd/pix.service %{buildroot}/usr/lib/systemd/user/pix.service
find %{buildroot} -exec touch -d "@$source_date_epoch" {} +

%files
/usr/bin/pix
/usr/share/doc/pix/README.md
/usr/lib/systemd/user/pix.service

%changelog
* $release_date Pix maintainers - $version-1
- Linux release
EOF
        cp "$archive" "$rpm_root/SOURCES/pix-$version-$target.tar.gz"
        rpm_log="$rpm_root/rpmbuild.log"
        if ! rpmbuild \
            --target "$rpm_arch" \
            --define "_topdir $rpm_root" \
            --define "_dbpath $rpm_root/RPMDB" \
            --define "use_source_date_epoch_as_buildtime 1" \
            --define "build_mtime_policy clamp_to_source_date_epoch" \
            -bb "$rpm_root/SPECS/pix.spec" >"$rpm_log" 2>&1; then
            cat "$rpm_log" >&2
            exit 1
        fi
        rpm_file=$(find "$rpm_root/RPMS" -type f -name '*.rpm' -print | sort | head -n 1)
        [ -n "$rpm_file" ] || {
            printf '%s\n' "rpmbuild produced no RPM artifact" >&2
            exit 1
        }
        cp "$rpm_file" "$output_dir/"
            printf '%s\n' "Wrote $output_dir/$(basename "$rpm_file")"
        else
            printf '%s\n' "rpmbuild is available for $rpm_host_arch but target is $rpm_arch; skipping cross-target rpm artifact."
        fi
    else
        printf '%s\n' "rpmbuild is unavailable; skipping rpm artifact."
    fi
fi

rm -Rf "$staging"
