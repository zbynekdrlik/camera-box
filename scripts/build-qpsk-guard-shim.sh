#!/usr/bin/env bash
# issue 1404 -- build the program-audio guard's QPSK marker decode shim (details in the header below).
set -euo pipefail

# Builds scripts/qpsk_guard_shim.cpp -- a C ABI over the EXISTING dock marker decoder in
# vendor/av-sync-dock/src/camera-box-marker-scan.hpp -- into a shared library that the dev1
# program-audio sampler (scripts/program_audio_sampler.py, via scripts/program_audio_marker.py) loads
# with ctypes. g++ only, never cargo (dev1 is Tier-0). The sampler reads MEASUREMENT only when this
# decoder finds the marker; without the library it reads UNKNOWN.
#
# Usage:
#   build-qpsk-guard-shim.sh [OUT]       build into OUT (default: the path below)
#   build-qpsk-guard-shim.sh --print-default
#
# Default OUT: ~/.local/lib/camera-box/libqpsk-guard-shim.so (outside the checkout, so the tree stays
# clean). The library is compiled into a temp file next to OUT and renamed over it: a running sampler
# keeps its mapped copy, never a half-written one (writing the mapped file in place can SIGBUS it).
#
# The build embeds the sha256 of the decoder sources (shim + the two headers, in that order) so the
# sampler can warn when it loads a library built from older sources. The Python side computes the
# same hash over the same list (program_audio_marker.SOURCES); a test pins the two together.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
INC="$ROOT/vendor/av-sync-dock/src"
DEFAULT_OUT="$HOME/.local/lib/camera-box/libqpsk-guard-shim.so"
SOURCES=(
  "$HERE/qpsk_guard_shim.cpp"
  "$INC/camera-box-audio.hpp"
  "$INC/camera-box-marker-scan.hpp"
)

if [[ "${1:-}" == "--print-default" ]]; then
  printf '%s\n' "$DEFAULT_OUT"
  exit 0
fi
if [[ $# -gt 1 ]]; then
  echo "usage: $0 [OUT] | --print-default" >&2
  exit 2
fi
OUT="${1:-$DEFAULT_OUT}"

command -v g++ >/dev/null 2>&1 || { echo "build-qpsk-guard-shim: g++ not found (install build-essential)" >&2; exit 1; }
for src in "${SOURCES[@]}"; do
  [[ -f "$src" ]] || { echo "build-qpsk-guard-shim: missing source $src" >&2; exit 1; }
done

SHA="$(cat "${SOURCES[@]}" | sha256sum | cut -d' ' -f1)"
mkdir -p "$(dirname "$OUT")"
TMP="$(mktemp "$(dirname "$OUT")/.libqpsk-guard-shim.XXXXXX")"
trap 'rm -f "$TMP"' EXIT

g++ -std=c++11 -O2 -fPIC -shared -Wall -Wextra -Werror \
  -DQPSK_GUARD_SOURCE_SHA256="\"$SHA\"" \
  -I "$INC" -o "$TMP" "$HERE/qpsk_guard_shim.cpp"
chmod 0644 "$TMP"
mv -f "$TMP" "$OUT"
trap - EXIT
echo "build-qpsk-guard-shim: built $OUT (sources sha256 $SHA)"
