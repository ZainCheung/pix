#!/bin/sh
set -eu

# Finalize a release directory after platform-specific jobs have finished.
# This step is intentionally target-agnostic: it creates metadata and one
# checksum manifest over every platform artifact collected in the directory.
#
# Usage:
#   packaging/release/finalize.sh [output-directory]

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
output_dir=${1:-"$repository_root/target/release-pkg"}

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
if [ -n "${PIX_RELEASE_VERSION:-}" ] && [ "$PIX_RELEASE_VERSION" != "$version" ]; then
    printf '%s\n' "PIX_RELEASE_VERSION=$PIX_RELEASE_VERSION does not match workspace version $version" >&2
    exit 1
fi

[ -d "$output_dir" ] || {
    printf '%s\n' "Release directory does not exist: $output_dir" >&2
    exit 1
}

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

packages = [
    {
        "name": package.get("name"),
        "version": package.get("version"),
        "license": package.get("license") or "unknown",
        "repository": package.get("repository"),
    }
    for package in metadata.get("packages", [])
]
packages.sort(key=lambda item: (item["name"] or "", item["version"] or ""))

spdx = {
    "spdxVersion": "SPDX-2.3",
    "dataLicense": "CC0-1.0",
    "SPDXID": "SPDXRef-DOCUMENT",
    "name": f"Pix {version}",
    "documentNamespace": f"https://pix.local/spdx/pix-{version}",
    "creationInfo": {
        "creators": ["Tool: pix packaging/release/finalize.sh"],
        "created": created,
    },
    "packages": [
        {
            "SPDXID": f"SPDXRef-{index}",
            "name": package["name"],
            "versionInfo": package["version"],
            "licenseDeclared": package["license"],
            "downloadLocation": package["repository"] or "NOASSERTION",
        }
        for index, package in enumerate(packages)
    ],
}
sbom_path = output_dir / f"pix-{version}-sbom.spdx.json"
sbom_path.write_text(json.dumps(spdx, indent=2) + "\n")
print(f"Wrote {sbom_path}")

license_path = output_dir / f"pix-{version}-licenses.txt"
with license_path.open("w") as handle:
    handle.write(f"Pix {version} dependency license report\n")
    handle.write(
        "Generated from Cargo metadata; review published crate license files "
        "for full terms.\n\n"
    )
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

command -v sha256sum >/dev/null 2>&1 || {
    printf '%s\n' "sha256sum is required to create release checksums" >&2
    exit 1
}

(
    cd "$output_dir"
    rm -f SHA256SUMS
    artifacts=$(find . -maxdepth 1 -type f \
        \( -name "pix-$version-*" -o -name "pix_${version}_*" \
        -o -name "pix-wire-$version-*" \) -print |
        sed 's#^\./##' | sort)
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

printf '%s\n' "Release metadata is in $output_dir"
