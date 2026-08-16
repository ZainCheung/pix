#!/bin/sh
set -eu

# Print the single product version shared by every workspace package.
#
# Cargo.toml is the source of truth.  cargo metadata is used instead of
# parsing TOML with a regular expression so release and packaging tooling
# continue to work if the manifest formatting changes.

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repository_root"

cargo metadata --no-deps --locked --format-version 1 |
    python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
packages_by_id = {package["id"]: package for package in metadata["packages"]}
workspace_packages = [
    packages_by_id[package_id]
    for package_id in metadata["workspace_members"]
    if package_id in packages_by_id
]

if not workspace_packages:
    raise SystemExit("workspace has no packages")

versions = sorted({package["version"] for package in workspace_packages})
if len(versions) != 1:
    names = ", ".join(
        "%s=%s" % (package["name"], package["version"])
        for package in sorted(workspace_packages, key=lambda item: item["name"])
    )
    raise SystemExit(f"workspace packages must share one version: {names}")

print(versions[0])
'
