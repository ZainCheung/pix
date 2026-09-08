#!/bin/sh
set -eu

# Convert a SemVer product version into the monotonic numeric version used by
# CFBundleVersion/Sparkle. The first three components are kept in fixed-width
# slots so that lexical and numeric comparisons agree for supported releases:
#
#   major * 1_000_000 + minor * 10_000 + patch * 100 + prerelease
#
# Stable releases use 99 as the final slot. A prerelease's final numeric
# identifier is used for the slot (beta.1 -> 01, beta.2 -> 02); identifiers
# without a number use 01. Build metadata does not affect the result.

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    printf '%s\n' "usage: scripts/macos-build-version.sh <semver>" >&2
    exit 64
fi

VERSION=$1 python3 - <<'PY'
import os
import re

version = os.environ["VERSION"]
match = re.fullmatch(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?",
    version,
)
if not match:
    raise SystemExit(f"invalid semantic version: {version}")

major, minor, patch, prerelease = match.groups()
major = int(major)
minor = int(minor)
patch = int(patch)

# Fixed-width slots keep the numeric representation unambiguous. Product
# versions outside these bounds should choose a larger encoding deliberately
# instead of silently colliding with an existing release.
if major > 999 or minor > 99 or patch > 99:
    raise SystemExit(
        "macOS build version supports major <= 999 and minor/patch <= 99"
    )

if prerelease is None:
    suffix = 99
else:
    numeric_identifiers = []
    for identifier in prerelease.split("."):
        if identifier.isdigit():
            if len(identifier) > 1 and identifier.startswith("0"):
                raise SystemExit(
                    "numeric prerelease identifiers cannot have leading zeros"
                )
            numeric_identifiers.append(int(identifier))
    suffix = numeric_identifiers[-1] if numeric_identifiers else 1
    if suffix < 1 or suffix > 98:
        raise SystemExit(
            "prerelease numeric identifier must be between 1 and 98"
        )

print(major * 1_000_000 + minor * 10_000 + patch * 100 + suffix)
PY
