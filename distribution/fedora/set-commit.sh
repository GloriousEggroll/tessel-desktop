#!/bin/bash
# Point specs at the GitHub commit their packages are built from.
#
#   distribution/fedora/set-commit.sh <commit> folder ...
#
# Sets %commit to the full hash and %vendorlock to that commit's Cargo.lock, so the
# vendored crates asset named in the spec matches. Raise Release: yourself.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
[ "$#" -ge 2 ] || { sed -n '4p' "$0" >&2; exit 2; }
commit=$(git -C "$repo" rev-parse --verify "$1^{commit}")
lock=$(git -C "$repo" show "$commit:Cargo.lock" | sha256sum | cut -c1-12)
shift
for folder in "$@"; do
    spec=$(echo "$here/${folder%/}"/*.spec)
    grep -q '^%{!?commit:' "$spec" || { echo "$spec has no sources" >&2; continue; }
    sed -i -e "s/^%{!?commit:%global commit [0-9a-f]*}/%{!?commit:%global commit $commit}/" \
           -e "s/^%{!?vendorlock:%global vendorlock [0-9a-f]*}/%{!?vendorlock:%global vendorlock $lock}/" "$spec"
    echo "$(basename "$spec" .spec): ${commit:0:12}, crates $lock"
done
echo "Upload tessel-desktop-vendor-$lock.tar.xz to the GitHub release vendor-$lock if it is not there yet."
