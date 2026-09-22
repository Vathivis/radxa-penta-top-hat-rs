#!/bin/sh
set -eu

package=radxa-penta-top-hat-rs
version=${1:?usage: generate-changelog.sh VERSION SOURCE_DATE_EPOCH [BASE_REF]}
source_date_epoch=${2:?usage: generate-changelog.sh VERSION SOURCE_DATE_EPOCH [BASE_REF]}
current_base_ref=${3:-}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
release_notes="$repo_root/packaging/release-notes.sh"
archive="$script_dir/changelog"
cd "$repo_root"

archived_version=$(sed -n "1s/^${package} (\([^)]*\)) .*/\1/p" "$archive")
[ -n "$archived_version" ] || {
    printf 'Cannot read the newest archived Debian changelog version\n' >&2
    exit 1
}

if [ "$archived_version" = "$version" ]; then
    sed -n '1,$p' "$archive"
    exit 0
fi

temporary=$(mktemp -d "${TMPDIR:-/tmp}/radxa-penta-changelog.XXXXXX")
trap 'rm -rf "$temporary"' 0 1 2 15
history="$temporary/version-history"
intermediate="$temporary/intermediate-releases"
intermediate_desc="$temporary/intermediate-releases-desc"

previous_version=
for commit in $(git rev-list --first-parent --reverse HEAD -- Cargo.toml); do
    commit_version=$(
        git show "${commit}:Cargo.toml" |
            sed -n 's/^version = "\([^"]*\)"/\1/p' |
            sed -n '1p'
    )
    if [ -n "$commit_version" ] && [ "$commit_version" != "$previous_version" ]; then
        printf '%s %s\n' "$commit_version" "$commit" >> "$history"
        previous_version=$commit_version
    fi
done

write_entry() {
    entry_version=$1
    base_ref=$2
    head_ref=$3
    entry_epoch=$4

    printf '%s (%s) stable; urgency=medium\n\n' "$package" "$entry_version"
    sh "$release_notes" "$base_ref" "$head_ref" | sed 's/^- /  * /'
    printf '\n -- Vathivis <vojtahumpl@seznam.cz>  %s\n' \
        "$(date -u -R -d "@$entry_epoch")"
}

# Walk forward so skipped, unpublished bumps remain in the next release's
# commit range instead of becoming artificial changelog boundaries.
previous_release_ref=
if git merge-base --is-ancestor "v${archived_version}" HEAD 2>/dev/null; then
    previous_release_ref="v${archived_version}"
fi
: > "$intermediate"
while read -r historical_version first_ref; do
    if ! dpkg --compare-versions "$historical_version" gt "$archived_version" ||
        ! dpkg --compare-versions "$historical_version" lt "$version"; then
        continue
    fi
    if [ -z "$previous_release_ref" ]; then
        previous_release_ref=$(git rev-parse "${first_ref}^{commit}^")
    fi

    tag="v${historical_version}"
    if ! git rev-parse --verify "${tag}^{commit}" >/dev/null 2>&1; then
        continue
    fi
    if ! git merge-base --is-ancestor "$tag" HEAD; then
        printf 'Tag %s is not an ancestor of the release commit\n' "$tag" >&2
        exit 1
    fi

    printf '%s %s %s\n' "$historical_version" "$previous_release_ref" "$tag" >> "$intermediate"
    previous_release_ref=$tag
done < "$history"

if [ -z "$current_base_ref" ]; then
    current_base_ref=$previous_release_ref
    if [ -z "$current_base_ref" ]; then
        current_first_ref=$(awk -v version="$version" '$1 == version { print $2; exit }' "$history")
        if [ -n "$current_first_ref" ]; then
            current_base_ref=$(git rev-parse "${current_first_ref}^{commit}^")
        fi
    fi
fi

write_entry "$version" "$current_base_ref" HEAD "$source_date_epoch"

awk '{ lines[NR] = $0 } END { for (line = NR; line >= 1; line--) print lines[line] }' \
    "$intermediate" > "$intermediate_desc"

while read -r historical_version prior_ref tag; do
    tag_epoch=$(git show -s --format=%ct "${tag}^{commit}")

    printf '\n'
    write_entry "$historical_version" "$prior_ref" "$tag" "$tag_epoch"
done < "$intermediate_desc"

printf '\n'
sed -n '1,$p' "$archive"
