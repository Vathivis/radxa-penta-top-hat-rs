#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

base_ref=${1:-}
head_ref=${2:-HEAD}
if [ -z "$base_ref" ]; then
    base_ref=$(git describe --tags --abbrev=0 --match 'v[0-9]*' "$head_ref" 2>/dev/null || true)
fi

git rev-parse --verify "${head_ref}^{commit}" >/dev/null
if [ -n "$base_ref" ]; then
    git rev-parse --verify "${base_ref}^{commit}" >/dev/null
    range="$base_ref..$head_ref"
else
    range=$head_ref
fi

git log --no-merges --reverse --format=%s "$range" |
    awk '
        /^chore: bump version( to)? / { next }
        NF { print "- " $0; printed = 1 }
        END { if (!printed) print "- Maintenance release" }
    '
