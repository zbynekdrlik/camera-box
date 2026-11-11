#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + one pure check, no other top-level statements) --
# sourcing runs in the CALLER's shell, so strict mode here would leak into it.
#
# scripts/lib/dsync-release.sh -- issue 1487: the ONE place the dsync release is named. dantesync was
# renamed dsync in its v1.25.0 (repository zbynekdrlik/dsync). The version itself stays
# DANTESYNC_VERSION_PIN in scripts/dantesync-version-gate.sh; a pin bump edits that line and the shas
# below together, and tests/python/test_dsync_swap_1487.py fails when they disagree.
#
# The shas are the release's own, read from `gh release view v1.29.0 -R zbynekdrlik/dsync`, so a release
# asset replaced after the pin is refused even with a matching `.sha256` beside it.

# Empty (a fixture release of its own) = no pinned sha.
DSYNC_PINNED_SHA_VERSION="${DSYNC_PINNED_SHA_VERSION-1.29.0}"
DSYNC_SHA256_LINUX="b07ac1dec7ad516fe74639a271cda8e5161febe76a02a1e6ccaeff6a46b8b6a7"
DSYNC_SHA256_WINDOWS="01285d593684b744cbc0b6439074dada32f180a66dbf55ce8aa47041a174d77d"
DSYNC_SHA256_TRAY_LINUX="c76a0cf9e571e7c1804b23e6d072d6c75e5c1c39f732a15b31d660b67b05f820"
DSYNC_SHA256_TRAY_WINDOWS="b61b9109997dbf9e5260d3b42d87c32a61217b2b05e67771251196b8f3ac4362"

# dsync_release_pinned_sha ASSET VERSION -> the pinned sha256 of ASSET (dsync-linux-amd64,
# dsync-windows-amd64.exe, dsync-tray-linux-amd64, dsync-tray-windows-amd64.exe) when VERSION is the
# pinned version; empty for any other version (a --target roll then has only the release's own
# `.sha256`) or an unknown asset. Always rc 0.
dsync_release_pinned_sha() {
  [ "${2:-}" = "$DSYNC_PINNED_SHA_VERSION" ] || return 0
  case "${1:-}" in
    dsync-linux-amd64) printf '%s' "$DSYNC_SHA256_LINUX" ;;
    dsync-windows-amd64.exe) printf '%s' "$DSYNC_SHA256_WINDOWS" ;;
    dsync-tray-linux-amd64) printf '%s' "$DSYNC_SHA256_TRAY_LINUX" ;;
    dsync-tray-windows-amd64.exe) printf '%s' "$DSYNC_SHA256_TRAY_WINDOWS" ;;
  esac
}

# dsync_release_sha_ok ASSET VERSION SHA -> rc 0 unless VERSION has a pinned sha for ASSET and SHA is
# not it (lower-cased first). A refusal prints one stderr line.
dsync_release_sha_ok() {
  local want have
  want="$(dsync_release_pinned_sha "${1:-}" "${2:-}")"
  [ -n "$want" ] || return 0
  have="$(printf '%s' "${3:-}" | tr '[:upper:]' '[:lower:]')"
  [ "$want" = "$have" ] && return 0
  echo "dsync-release: $1 v$2 has sha256 '${have}', the pin says '${want}' -- refusing it" >&2
  return 1
}
