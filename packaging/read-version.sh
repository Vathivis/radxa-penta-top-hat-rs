#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml" | sed -n '1p')
[ -n "$version" ] || {
    printf 'Could not read the package version from Cargo.toml\n' >&2
    exit 1
}

printf '%s\n' "$version"
