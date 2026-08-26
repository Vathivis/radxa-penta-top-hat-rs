#!/bin/sh
set -eu

package=radxa-penta-top-hat-rs

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

manifest_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | sed -n '1p')
lock_version=$(
    awk -v wanted="$package" '
        $0 == "[[package]]" {
            matched = 0
            next
        }
        /^name = "/ {
            name = $0
            sub(/^name = "/, "", name)
            sub(/"$/, "", name)
            matched = (name == wanted)
            next
        }
        matched && /^version = "/ {
            version = $0
            sub(/^version = "/, "", version)
            sub(/"$/, "", version)
            print version
            exit
        }
    ' Cargo.lock
)
changelog_version=$(
    sed -n "1s/^${package} (\([^)]*\)) .*/\1/p" packaging/debian/changelog
)

[ -n "$manifest_version" ] || {
    printf 'Could not read the package version from Cargo.toml\n' >&2
    exit 1
}
[ -n "$lock_version" ] || {
    printf 'Could not read the %s version from Cargo.lock\n' "$package" >&2
    exit 1
}
[ -n "$changelog_version" ] || {
    printf 'Could not read the package version from packaging/debian/changelog\n' >&2
    exit 1
}

[ "$manifest_version" = "$lock_version" ] || {
    printf 'Version mismatch: Cargo.toml=%s Cargo.lock=%s\n' \
        "$manifest_version" "$lock_version" >&2
    exit 1
}
[ "$manifest_version" = "$changelog_version" ] || {
    printf 'Version mismatch: Cargo.toml=%s Debian changelog=%s\n' \
        "$manifest_version" "$changelog_version" >&2
    exit 1
}

printf '%s\n' "$manifest_version"
