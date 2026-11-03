#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + one pure check, no other top-level statements) --
# sourcing runs in the CALLER's shell, so strict mode here would leak into it.
#
# scripts/lib/dsync-release.sh -- issue 1487: the ONE place the dsync release is named. dantesync was
# renamed dsync in its v1.25.0 (repository zbynekdrlik/dsync). The version itself stays
# DANTESYNC_VERSION_PIN in scripts/dantesync-version-gate.sh; a pin bump edits that line and the shas
# below together, and tests/python/test_dsync_swap_1487.py fails when they disagree.
#
# The shas are the release's own, read from `gh release view v1.27.0 -R zbynekdrlik/dsync`, so a release
# asset replaced after the pin is refused even with a matching `.sha256` beside it.

# Empty (a fixture release of its own) = no pinned sha.
DSYNC_PINNED_SHA_VERSION="${DSYNC_PINNED_SHA_VERSION-1.27.0}"
DSYNC_SHA256_LINUX="4fadc408e025d24a3517652a2ded1936ed1895de516d9315e20e49491e2fe82a"
DSYNC_SHA256_WINDOWS="9a93e1b5a7e5d3a201367f15fbb6c528e1459e0ae25ff8adf49501f99ad37848"
DSYNC_SHA256_TRAY_LINUX="93f1cc7a8bd0976f5cee9b8ce37131a8755c00ab13b016cc79a95aa8f45b0227"
DSYNC_SHA256_TRAY_WINDOWS="ede03005a212c87e99047b193dc3e5875c3da5beb32aefe5355df664448219ab"

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
