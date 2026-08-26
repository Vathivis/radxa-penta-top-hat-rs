#!/bin/sh
set -eu

package=radxa-penta-top-hat-rs
target=${RUST_TARGET:-aarch64-unknown-linux-musl}

case "$target" in
    aarch64-unknown-linux-*) architecture=arm64 ;;
    *)
        printf 'Unsupported release target: %s\n' "$target" >&2
        exit 2
        ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

version=$(sh "$script_dir/check-version.sh")
target_dir=${CARGO_TARGET_DIR:-$repo_root/target/debian-build-v$version}
dist_dir=${DIST_DIR:-$repo_root/dist}

RUST_TARGET=$target \
    CARGO_TARGET_DIR=$target_dir \
    DIST_DIR=$dist_dir \
    sh "$script_dir/debian/build-deb.sh"

built_binary="$target_dir/$target/release/$package"
standalone_name="${package}-v${version}-${target}"
archive_name="${standalone_name}.tar.gz"
deb_name="${package}_${version}_${architecture}.deb"

install -m 0755 "$built_binary" "$dist_dir/$standalone_name"

source_date_epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}
temporary_tar=$(mktemp "${TMPDIR:-/tmp}/radxa-penta-release.XXXXXX.tar")
trap 'rm -f "$temporary_tar"' 0 1 2 15

(
    cd "$dist_dir"
    tar \
        --sort=name \
        --mtime="@$source_date_epoch" \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        -cf "$temporary_tar" \
        "$standalone_name"
    gzip -9n -c "$temporary_tar" > "$archive_name"
    chmod 0644 "$archive_name"
    sha256sum "$standalone_name" "$archive_name" "$deb_name" > SHA256SUMS
    chmod 0644 SHA256SUMS
)

printf 'Release artifacts for v%s:\n' "$version"
printf '  %s\n' \
    "$dist_dir/$standalone_name" \
    "$dist_dir/$archive_name" \
    "$dist_dir/$deb_name" \
    "$dist_dir/SHA256SUMS"
