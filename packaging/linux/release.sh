#!/bin/sh
set -eu

# Reproducible Linux release workflow.
#
# Usage:
#   packaging/linux/release.sh [target-triple ...]
#
# Defaults to x86_64-unknown-linux-gnu and aarch64-unknown-linux-gnu.
# Produces tar.gz archives, deb/rpm packages when their native tools exist,
# an SPDX SBOM, a dependency license report, SHA-256 checksums, and optional
# detached signatures when PIX_SIGNING_KEY is set.

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
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
case "$version" in
    ''|*[!0-9A-Za-z.+-]*)
        printf '%s\n' "Invalid workspace version for a Linux release: $version" >&2
        exit 1
        ;;
esac

for target in "$@"; do
    packaging/linux/build-release.sh "$target" "$output_dir"
    PIX_PACKAGE_DEB=${PIX_PACKAGE_DEB:-1} \
        PIX_PACKAGE_RPM=${PIX_PACKAGE_RPM:-1} \
        packaging/linux/package.sh "$target" "$output_dir"
done

# SBOM and license report are generated from Cargo metadata and use the same
# SOURCE_DATE_EPOCH as the archives and native packages.
python3 - "$output_dir" "$version" <<'PY'
import datetime
import json
import os
import subprocess
import sys
from pathlib import Path

output_dir = Path(sys.argv[1])
version = sys.argv[2]
epoch = int(os.environ["SOURCE_DATE_EPOCH"])
created = datetime.datetime.fromtimestamp(
    epoch, tz=datetime.timezone.utc
).strftime("%Y-%m-%dT%H:%M:%SZ")

metadata = json.loads(
    subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    ).stdout
)

packages = []
for package in metadata.get("packages", []):
    packages.append({
        "name": package.get("name"),
        "version": package.get("version"),
        "license": package.get("license") or "unknown",
        "repository": package.get("repository"),
    })
packages.sort(key=lambda item: (item["name"] or "", item["version"] or ""))

spdx = {
    "spdxVersion": "SPDX-2.3",
    "dataLicense": "CC0-1.0",
    "SPDXID": "SPDXRef-DOCUMENT",
    "name": f"Pix {version}",
    "documentNamespace": f"https://pix.local/spdx/pix-{version}",
    "creationInfo": {
        "creators": ["Tool: pix packaging/linux/release.sh"],
        "created": created,
    },
    "packages": [
        {
            "SPDXID": f"SPDXRef-{i}",
            "name": package["name"],
            "versionInfo": package["version"],
            "licenseDeclared": package["license"],
            "downloadLocation": package["repository"] or "NOASSERTION",
        }
        for i, package in enumerate(packages)
    ],
}
sbom_path = output_dir / f"pix-{version}-sbom.spdx.json"
sbom_path.write_text(json.dumps(spdx, indent=2) + "\n")
print(f"Wrote {sbom_path}")

license_path = output_dir / f"pix-{version}-licenses.txt"
with license_path.open("w") as handle:
    handle.write(f"Pix {version} dependency license report\n")
    handle.write("Generated from Cargo metadata; review published crate license files for full terms.\n\n")
    for package in packages:
        handle.write(
            f"{package['name']} {package['version']}  "
            f"{package['license']}  {package['repository'] or 'NOASSERTION'}\n"
        )
print(f"Wrote {license_path}")
PY

sbom="$output_dir/pix-$version-sbom.spdx.json"
licenses="$output_dir/pix-$version-licenses.txt"
[ -s "$sbom" ] || {
    printf '%s\n' "SBOM was not created or is empty" >&2
    exit 1
}
[ -s "$licenses" ] || {
    printf '%s\n' "License report was not created or is empty" >&2
    exit 1
}

# Include every release artifact for this version, including metadata and
# packages. The previous checksum glob did not match the actual names.
if ! command -v sha256sum >/dev/null 2>&1; then
    printf '%s\n' "sha256sum is required to create release checksums" >&2
    exit 1
fi
(
    cd "$output_dir"
    rm -f SHA256SUMS
    artifacts=$(find . -maxdepth 1 -type f \
        \( -name "pix-$version-*" -o -name "pix_${version}_*" \) \
        ! -name '*.sig' -printf '%f\n' | sort)
    [ -n "$artifacts" ] || {
        printf '%s\n' "No release artifacts found; refusing to write empty SHA256SUMS" >&2
        exit 1
    }
    : > SHA256SUMS
    while IFS= read -r artifact; do
        [ -n "$artifact" ] || continue
        sha256sum -- "$artifact" >> SHA256SUMS
    done <<EOF
$artifacts
EOF
    [ -s SHA256SUMS ] || {
        printf '%s\n' "SHA256SUMS is empty" >&2
        exit 1
    }
    printf '%s\n' "Wrote $output_dir/SHA256SUMS"
)

# Optional detached signatures. The signing key is supplied by CI or the
# release owner; no key material is ever committed.
if [ -n "${PIX_SIGNING_KEY:-}" ]; then
    command -v gpg >/dev/null 2>&1 || {
        printf '%s\n' "PIX_SIGNING_KEY is set but gpg is unavailable" >&2
        exit 1
    }
    (
        cd "$output_dir"
        rm -f SHA256SUMS.sig
        gpg --batch --yes --local-user "$PIX_SIGNING_KEY" \
            --detach-sign --armor --output SHA256SUMS.sig SHA256SUMS
        [ -s SHA256SUMS.sig ] || {
            printf '%s\n' "gpg produced an empty checksum signature" >&2
            exit 1
        }
        printf '%s\n' "Wrote $output_dir/SHA256SUMS.sig"
    )
else
    printf '%s\n' "PIX_SIGNING_KEY is not set; skipping detached signature."
fi

printf '%s\n' "Release artifacts are in $output_dir"
