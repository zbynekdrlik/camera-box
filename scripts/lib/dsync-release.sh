#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + one pure check, no other top-level statements) --
# sourcing runs in the CALLER's shell, so strict mode here would leak into it.
#
# scripts/lib/dsync-release.sh -- issue 1487: the ONE place the dsync release is named. dantesync was
# renamed dsync in its v1.25.0 (repository zbynekdrlik/dsync). The version itself stays
# DANTESYNC_VERSION_PIN in scripts/dantesync-version-gate.sh; a pin bump edits that line and the shas
# below together, and tests/python/test_dsync_swap_1487.py fails when they disagree.
#
# The shas are the release's own, read from `gh release view v1.35.2 -R zbynekdrlik/dsync`, so a release
# asset replaced after the pin is refused even with a matching `.sha256` beside it.

# Empty (a fixture release of its own) = no pinned sha.
DSYNC_PINNED_SHA_VERSION="${DSYNC_PINNED_SHA_VERSION-1.35.2}"
DSYNC_SHA256_LINUX="8486c9891902500f78c65e871818716a498eb7163ca4d7d23a9dc26d066b874c"
DSYNC_SHA256_WINDOWS="37e59a276a6559df4d0980acb42c6a652a1cc1a8de91d8fcd3d868cc95c56e22"
DSYNC_SHA256_TRAY_LINUX="7a3ee1f4df8a8d8eb8bd2eeab85017609b3287064df4d1d27f25eb2fd37fda07"
DSYNC_SHA256_TRAY_WINDOWS="12e692911f255f5ce715a017016a34944da4b0aa6627431b38a40f7024384255"

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
