#!/bin/sh
set -eu

package=radxa-penta-top-hat-rs
target=${RUST_TARGET:-aarch64-unknown-linux-musl}

case "$target" in
    aarch64-unknown-linux-*) architecture=arm64 ;;
    *)
        printf 'Unsupported Debian release target: %s\n' "$target" >&2
        exit 2
        ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
cd "$repo_root"

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | sed -n '1p')
[ -n "$version" ] || {
    printf 'Could not read the package version from Cargo.toml\n' >&2
    exit 1
}

target_dir=${CARGO_TARGET_DIR:-$repo_root/target/debian-build-v$version}
dist_dir=${DIST_DIR:-$repo_root/dist}
export CARGO_TARGET_DIR=$target_dir

cargo build --locked --release --target "$target"
binary="$target_dir/$target/release/$package"
expected_version="$package $version"
reported_version=$("$binary" --version)
[ "$reported_version" = "$expected_version" ] || {
    printf 'Binary version mismatch: expected %s, got %s\n' \
        "$expected_version" "$reported_version" >&2
    exit 1
}

temporary=$(mktemp -d "${TMPDIR:-/tmp}/radxa-penta-deb.XXXXXX")
trap 'rm -rf "$temporary"' 0 1 2 15
root="$temporary/root"
docs="$root/usr/share/doc/$package"

install -d -m 0755 \
    "$root/DEBIAN" \
    "$root/etc" \
    "$root/usr/bin" \
    "$root/usr/lib/systemd/system" \
    "$root/usr/lib/$package" \
    "$docs/examples"

install -m 0755 "$binary" "$root/usr/bin/$package"
install -m 0644 "$script_dir/rockpi-penta.conf" "$root/etc/rockpi-penta.conf"
install -m 0644 "$script_dir/radxa-penta-top-hat-rs.service" \
    "$root/usr/lib/systemd/system/radxa-penta-top-hat-rs.service"
install -m 0755 "$script_dir/configure-board" "$root/usr/lib/$package/configure-board"
install -m 0755 "$script_dir/validate-config" "$root/usr/lib/$package/validate-config"
install -m 0644 "$script_dir/rockpi-penta.env.example" \
    "$docs/examples/rockpi-penta.env"
install -m 0644 README.md "$docs/README.md"
gzip -9n "$docs/README.md"
install -m 0644 LICENSE "$docs/copyright"
install -m 0644 THIRD_PARTY_LICENSES.md "$docs/THIRD_PARTY_LICENSES.md"
install -m 0644 "$script_dir/changelog" "$docs/changelog.Debian"
gzip -9n "$docs/changelog.Debian"

install -m 0644 "$script_dir/conffiles" "$root/DEBIAN/conffiles"
install -m 0755 "$script_dir/postinst" "$root/DEBIAN/postinst"
install -m 0755 "$script_dir/prerm" "$root/DEBIAN/prerm"
install -m 0755 "$script_dir/postrm" "$root/DEBIAN/postrm"

installed_size=$(du -sk "$root" | awk '{print $1}')
sed \
    -e "s/@VERSION@/$version/g" \
    -e "s/@ARCH@/$architecture/g" \
    -e "s/@INSTALLED_SIZE@/$installed_size/g" \
    "$script_dir/control.in" > "$root/DEBIAN/control"
chmod 0644 "$root/DEBIAN/control"

(
    cd "$root"
    find etc usr -type f -print | LC_ALL=C sort | xargs md5sum
) > "$root/DEBIAN/md5sums"
chmod 0644 "$root/DEBIAN/md5sums"

source_date_epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}
export SOURCE_DATE_EPOCH=$source_date_epoch
find "$root" -exec touch -h -d "@$source_date_epoch" {} +

install -d -m 0755 "$dist_dir"
output="$dist_dir/${package}_${version}_${architecture}.deb"
dpkg-deb --root-owner-group -Zxz --build "$root" "$output"
chmod 0644 "$output"

checksum=$(sha256sum "$output" | awk '{print $1}')
printf '%s  %s\n' "$checksum" "$(basename -- "$output")" \
    > "${output}.sha256"
chmod 0644 "${output}.sha256"

printf 'Built %s\n' "$output"
printf 'Checksum: %s\n' "$checksum"
