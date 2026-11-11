#!/bin/bash
# Build every Tessel RPM in a clean Nobara mock chroot.
#
#   distribution/fedora/build.sh [folder ...]     all folders, or only the named ones
#   TREE=1 distribution/fedora/build.sh ...       the working tree instead of each spec's commit
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
# gcc needs kernel-headers, which only Nobara's kernel repository has and the
# mock configuration does not list.
kernel_repo=${KERNEL_REPO:-https://usw.nobaraproject.org/rolling/linux-nobara-mainline/x86_64/}
mock=(mock -r "$config" --uniqueext tessel --quiet --addrepo="$kernel_repo")
target=/var/tmp/tessel-target

mkdir -p "$out/RPMS" "$out/SRPMS" "$out/logs" "$sources"

# Sources. Each spec names the GitHub commit it builds and the vendored crates for
# that commit's Cargo.lock; both are made here from the local repository under the
# names GitHub serves, so builds need no network and match what COPR fetches.
# TREE=1 builds the working tree instead, including uncommitted changes to tracked
# files, by overriding the spec's commit (for testing only: never submit those SRPMs).
defines=()
if [ -n "${TREE:-}" ]; then
    tree=$(git -C "$repo" stash create)
    tree=${tree:-$(git -C "$repo" rev-parse HEAD)}
    defines=(--define "commit $tree" --define "vendorlock $(sha256sum "$repo/Cargo.lock" | cut -c1-12)")
fi

# The sources of a spec, as rpm sees them with the overrides above.
spec_sources() { rpmspec -P "${defines[@]}" "$1" | sed -n 's/^Source[0-9]*:[[:space:]]*//p'; }

make_sources() {
    local url commit lock work path
    for url in $(spec_sources "$1"); do
        case $url in
        */raw/*)
            # One file of the repository at a commit (the wallpaper's pictures).
            # Its name does not say which commit, so it is always made again.
            path=${url#*/raw/}; commit=${path%%/*}; path=${path#*/}
            git -C "$repo" cat-file -e "$commit:$path" 2>/dev/null ||
                { echo "$path at $commit (from $1) is not in this repository" >&2; exit 1; }
            git -C "$repo" show "$commit:$path" > "$sources/${url##*/}"
            ;;
        */archive/*)
            commit=${url%/*}; commit=${commit##*/}
            [ -f "$sources/${url##*/}" ] && continue
            git -C "$repo" cat-file -e "$commit^{commit}" 2>/dev/null ||
                { echo "commit $commit (from $1) is not in this repository" >&2; exit 1; }
            git -C "$repo" archive --prefix="tessel-desktop-$commit/" -o "$sources/${url##*/}" "$commit"
            ;;
        */releases/download/*)
            # The vendored crates. The Windows import libraries are large and never
            # used on Linux; cargo only verifies crates it builds.
            [ -f "$sources/${url##*/}" ] && continue
            lock=${url##*-}; lock=${lock%.tar.xz}
            commit=$(spec_sources "$1" | sed -n 's|.*/archive/\([^/]*\)/.*|\1|p')
            work=$(mktemp -d -p "$sources")
            if [ "$(sha256sum "$repo/Cargo.lock" | cut -c1-12)" = "$lock" ]; then
                (cd "$repo" && cargo vendor --locked --versioned-dirs "$work/vendor" >/dev/null)
            else
                mkdir "$work/src"
                git -C "$repo" archive "$commit" | tar -C "$work/src" -x
                [ "$(sha256sum "$work/src/Cargo.lock" | cut -c1-12)" = "$lock" ] ||
                    { echo "$1: vendorlock $lock is not the Cargo.lock of $commit" >&2; exit 1; }
                (cd "$work/src" && cargo vendor --locked --versioned-dirs "$work/vendor" >/dev/null)
            fi
            find "$work/vendor" -path '*/windows*' \( -name '*.a' -o -name '*.lib' \) -delete
            tar -C "$work" -cJf "$sources/${url##*/}" vendor
            rm -rf "$work"
            ;;
        esac
    done
}

# Component folders first (tessel-common has no compiled code), tessel-meta last.
if [ "$#" -gt 0 ]; then
    folders=("$@")
else
    mapfile -t folders < <(cd "$here" && ls -d */ | tr -d / | grep -v '^tessel-meta$')
    folders+=(tessel-meta)
fi

# Each package gets a clean chroot, as on COPR: a shared one hides missing
# BuildRequires, because an earlier package has already installed them.
# FAST=1 shares one chroot and its compiled crates for a quick local check only.
fast=${FAST:-}
if [ -n "$fast" ]; then
    "${mock[@]}" --clean
    "${mock[@]}" --init
fi
for folder in "${folders[@]}"; do
    spec=$(echo "$here/$folder"/*.spec)
    name=$(basename "$spec" .spec)
    echo "==> $name"
    rm -f "$out/SRPMS/$name-$version-"*.src.rpm "$out/RPMS/$name-$version-"*.rpm
    make_sources "$spec"
    rpmbuild -bs "$spec" "${defines[@]}" --define "_sourcedir $sources" --define "_srcrpmdir $out/SRPMS" --define "dist .fc44" >/dev/null
    rm -rf "${out:?}/logs/$name"
    if [ -n "$fast" ]; then
        "${mock[@]}" "${defines[@]}" --no-clean --no-cleanup-after --define "tessel_cargo_target_dir $target" \
            --resultdir "$out/logs/$name" --rebuild "$out/SRPMS/$name-$version-"*.src.rpm
    else
        "${mock[@]}" "${defines[@]}" --resultdir "$out/logs/$name" --rebuild "$out/SRPMS/$name-$version-"*.src.rpm
    fi
    mv "$out/logs/$name/"*.rpm "$out/RPMS/" 2>/dev/null || :
    rm -f "$out/RPMS/"*.src.rpm
done

[ "$#" -gt 0 ] || "$here/check.sh"
