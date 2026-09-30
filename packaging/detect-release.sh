#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

before_ref=${1:-}
case "$before_ref" in
    *[!0]*) ;;
    *)
        printf 'Cannot determine the previous main-branch revision\n' >&2
        exit 1
        ;;
esac

version=$(sh "$script_dir/read-version.sh")
previous_manifest=$(git show "${before_ref}:Cargo.toml")
previous_version=$(printf '%s\n' "$previous_manifest" |
    sed -n 's/^version = "\([^"]*\)"/\1/p' | sed -n '1p')
[ -n "$previous_version" ] || {
    printf 'Cannot read the package version at %s\n' "$before_ref" >&2
    exit 1
}
if [ "$version" != "$previous_version" ] &&
    ! dpkg --compare-versions "$version" gt "$previous_version"; then
    printf 'Version changed from %s to %s, but it was not increased\n' \
        "$previous_version" "$version" >&2
    exit 1
fi

should_release=false
base_ref=$before_ref
# A failed lockfile push may leave the next main commit at the same version.
# Retry until publication creates its release tag, not just on version changes.
if ! git rev-parse --verify --quiet "refs/tags/v${version}^{commit}" >/dev/null; then
    should_release=true
    latest_tag=$(git describe --tags --abbrev=0 --match 'v[0-9]*' "$before_ref" 2>/dev/null || true)
    if [ -n "$latest_tag" ]; then
        base_ref=$latest_tag
    fi
fi

printf 'version=%s\nshould_release=%s\nbase_ref=%s\n' \
    "$version" "$should_release" "$base_ref"
