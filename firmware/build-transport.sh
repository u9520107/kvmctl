#!/usr/bin/env bash
set -euo pipefail

firmware=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
peer_source=${1:?Usage: bash firmware/build-transport.sh PEER_CHECKOUT [menuconfig]}
shift
if (($# == 0)); then
    set -- reconfigure build
fi

if ! idf_version=$(idf.py --version) || [[ "$idf_version" != "ESP-IDF v6.1.1" ]]; then
    echo "Activate ESP-IDF v6.1.1 for esp32s3 before running this script." >&2
    exit 1
fi

peer_revision=3971094e2e7c2ab7cb4de59e4eae710e1902d2b4
if [[ $(git -C "$peer_source" rev-parse HEAD) != "$peer_revision" ]]; then
    echo "Expected esp-webrtc-solution at $peer_revision." >&2
    exit 1
fi
libsrtp_source=${KVMCTL_LIBSRTP_SOURCE:-"$peer_source/components/esp_peer/examples/peer_demo/managed_components/espressif__esp_libsrtp"}
if [[ ! -f "$libsrtp_source/CMakeLists.txt" ]]; then
    echo "Set KVMCTL_LIBSRTP_SOURCE to the espressif__esp_libsrtp directory for version 1.0.0." >&2
    exit 1
fi
KVMCTL_LIBSRTP_SOURCE=$(realpath -- "$libsrtp_source")
if [[ ${KVMCTL_LIBSRTP_SOURCE##*/} != espressif__esp_libsrtp ]]; then
    echo "Name the libSRTP component directory espressif__esp_libsrtp for ESP-IDF component discovery." >&2
    exit 1
fi
export KVMCTL_LIBSRTP_SOURCE

build_dir="$firmware/build-wsl"
mkdir -p -- "$build_dir"
exec 9>"$build_dir/.build.lock"
flock 9

patch="$firmware/../verification/m0/peer-fingerprint/esp-peer-fingerprint.patch"
patch_hash=$(sha256sum < "$patch")
vendor="$build_dir/peer-$peer_revision-${patch_hash%% *}"
if [[ ! -d "$vendor" ]]; then
    staging=$(mktemp -d "$build_dir/peer.XXXXXXXX")
    trap 'rm -rf -- "$staging"' EXIT
    git -C "$peer_source" archive "$peer_revision" components/esp_peer | tar -xf - -C "$staging"
    # Apply from the archive root without discovering the outer checkout.
    GIT_CEILING_DIRECTORIES="$build_dir" git -C "$staging" apply "$patch"
    mv -- "$staging" "$vendor"
    trap - EXIT
fi
GIT_CEILING_DIRECTORIES="$build_dir" git -C "$vendor" apply --reverse --check "$patch"
export KVMCTL_PEER_SOURCE="$vendor/components/esp_peer"

source_hash=$(printf '%s\n' "$KVMCTL_PEER_SOURCE" "$KVMCTL_LIBSRTP_SOURCE" | sha256sum)
idf.py -C "$firmware" -B "$build_dir" --no-ccache \
    -DIDF_TARGET=esp32s3 -DSDKCONFIG="$build_dir/sdkconfig" \
    -DKVMCTL_DEPENDENCIES_LOCK="$build_dir/dependencies-${source_hash%% *}.lock" "$@"
