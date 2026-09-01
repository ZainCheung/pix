#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/pix-ci-paths.XXXXXX")
fixture_repository="$fixture_root/repository"
output_path="$fixture_root/github-output"
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM

mkdir -p "$fixture_repository/.github/scripts"
cp "$repository_root/.github/scripts/detect-ci-changes.sh" \
    "$fixture_repository/.github/scripts/detect-ci-changes.sh"

git -C "$fixture_repository" init -q
git -C "$fixture_repository" config user.email "ci@example.invalid"
git -C "$fixture_repository" config user.name "Pix CI"
printf 'fixture\n' > "$fixture_repository/README.md"
git -C "$fixture_repository" add .
git -C "$fixture_repository" commit -qm "fixture baseline"
base_sha=$(git -C "$fixture_repository" rev-parse HEAD)

assert_output() {
    key=$1
    expected=$2
    if ! grep -qx "$key=$expected" "$output_path"; then
        printf 'expected %s=%s for %s\n' "$key" "$expected" "$case_path" >&2
        cat "$output_path" >&2
        exit 1
    fi
}

run_case() {
    case_path=$1
    expected_relay=$2
    expected_deploy=$3

    git -C "$fixture_repository" reset --hard -q "$base_sha"
    git -C "$fixture_repository" clean -fdq
    mkdir -p "$fixture_repository/$(dirname "$case_path")"
    printf 'fixture\n' > "$fixture_repository/$case_path"
    git -C "$fixture_repository" add "$case_path"
    git -C "$fixture_repository" commit -qm "test $case_path"
    head_sha=$(git -C "$fixture_repository" rev-parse HEAD)
    : > "$output_path"

    (
        cd "$fixture_repository"
        GITHUB_OUTPUT="$output_path" \
        GITHUB_EVENT_NAME=push \
        GITHUB_EVENT_BEFORE="$base_sha" \
        GITHUB_SHA="$head_sha" \
        sh .github/scripts/detect-ci-changes.sh
    )

    assert_output relay "$expected_relay"
    assert_output relay_deploy "$expected_deploy"
}

run_case relay/README.md true false
run_case relay/src/index.ts true true
run_case 'docs/(use-pix)/REMOTE_ACCESS.md' false false

printf 'CI path classification tests passed.\n'
