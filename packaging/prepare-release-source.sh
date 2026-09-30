#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

version=$(sh "$script_dir/read-version.sh")
# Update workspace metadata while retaining existing dependency versions.
cargo update --workspace
if ! git diff --quiet HEAD -- Cargo.lock; then
    git add -- Cargo.lock
    # A rerun from the same source revision must prepare the same commit.
    source_date=$(git show -s --format=%cI HEAD)
    GIT_AUTHOR_DATE="$source_date" GIT_COMMITTER_DATE="$source_date" \
        git -c user.name='github-actions[bot]' \
        -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
        -c commit.gpgsign=false commit --quiet --only \
        -m 'chore: synchronize release lockfile' \
        -m "- Update Cargo.lock to match Cargo.toml for release $version" \
        -- Cargo.lock
fi
git rev-parse HEAD
