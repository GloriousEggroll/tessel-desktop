#!/bin/bash
# Build every Tessel RPM in a clean Nobara mock chroot.
#
#   distribution/fedora/build.sh [folder ...]     all folders, or only the named ones
#
# Results: build-fedora/RPMS, build-fedora/SRPMS, build-fedora/logs/<package>.
# MOCK_CONFIG picks the mock configuration (default: nobara-44-x86_64 in rpm-sources).
# Nothing on this system is installed, started or changed; only mock's own chroot.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=$repo/build-fedora
sources=$out/sources
config=${MOCK_CONFIG:-/mnt/Storage/Projects/Development/nobara/rpm-sources/mock/nobara-44-x86_64.cfg}
version=$(rpmspec -q --srpm --qf '%{version}' "$here/tessel-meta/tessel-meta.spec")
# One chroot and one cargo target directory, so each package reuses what the
# packages before it compiled.
# gcc needs kernel-headers, which only Nobara's kernel repository has and the
# mock configuration does not list.
kernel_repo=${KERNEL_REPO:-https://usw.nobaraproject.org/rolling/linux-nobara-mainline/x86_64/}
mock=(mock -r "$config" --uniqueext tessel --quiet --addrepo="$kernel_repo")
target=/var/tmp/tessel-target

mkdir -p "$out/RPMS" "$out/SRPMS" "$out/logs" "$sources"

# The working tree, including uncommitted changes to tracked files.
tree=$(git -C "$repo" stash create)
git -C "$repo" archive --prefix="tessel-$version/" -o "$sources/tessel-$version.tar.gz" "${tree:-HEAD}"

# Vendored crates, remade only when Cargo.lock changes. The Windows import
# libraries are large and never used on Linux; cargo only verifies crates it builds.
vendor=$sources/tessel-$version-vendor.tar.xz
lock=$(sha256sum "$repo/Cargo.lock" | cut -d' ' -f1)
if [ ! -f "$vendor" ] || [ "$(cat "$vendor.lock" 2>/dev/null)" != "$lock" ]; then
    work=$(mktemp -d -p "$sources")
    (cd "$repo" && cargo vendor --locked --versioned-dirs "$work/vendor" >/dev/null)
    find "$work/vendor" -path '*/windows*' \( -name '*.a' -o -name '*.lib' \) -delete
    tar -C "$work" -cJf "$vendor" vendor
    rm -rf "$work"
    echo "$lock" > "$vendor.lock"
fi

# Component folders first (tessel-common has no compiled code), tessel-meta last.
if [ "$#" -gt 0 ]; then
    folders=("$@")
else
    mapfile -t folders < <(cd "$here" && ls -d */ | tr -d / | grep -v '^tessel-meta$')
    folders+=(tessel-meta)
fi

# A partial build keeps the chroot and its compiled crates; a full one starts clean.
if [ "$#" -eq 0 ] || [ ! -d "/var/lib/mock/nobara-44-x86_64-tessel/root" ]; then
    "${mock[@]}" --clean
    "${mock[@]}" --init
fi
for folder in "${folders[@]}"; do
    spec=$(echo "$here/$folder"/*.spec)
    name=$(basename "$spec" .spec)
    echo "==> $name"
    rm -f "$out/SRPMS/$name-$version-"*.src.rpm "$out/RPMS/$name-$version-"*.rpm
    rpmbuild -bs "$spec" --define "_sourcedir $sources" --define "_srcrpmdir $out/SRPMS" --define "dist .fc44" >/dev/null
    rm -rf "${out:?}/logs/$name"
    "${mock[@]}" --no-clean --no-cleanup-after --define "tessel_cargo_target_dir $target" \
        --resultdir "$out/logs/$name" --rebuild "$out/SRPMS/$name-$version-"*.src.rpm
    mv "$out/logs/$name/"*.rpm "$out/RPMS/" 2>/dev/null || :
    rm -f "$out/RPMS/"*.src.rpm
done

[ "$#" -gt 0 ] || "$here/check.sh"
