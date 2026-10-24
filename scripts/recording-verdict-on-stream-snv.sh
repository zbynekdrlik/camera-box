#!/usr/bin/env bash
# recording-verdict-on-stream-snv.sh — the [8/8b] stream recording decode ON the Linux stream box (header below).
set -euo pipefail
#
# issue 1462 (owner ROZHODNUTÉ 6377910161, "nikdy to nesmie byt skipped"): since 22.10.2026 the stream
# box (10.77.9.204) is the Linux stream-snv, the same OBS appliance as strih-snv. The full-path E2E
# must keep its QR + audio-marker A/V measurement BLOCKING there, and that measurement rides the
# stream partial: `recording-verdict --extract-partial stream --av-marker-log …` decodes the stream
# recording (the one recording that holds the cam2 QR and the marker audio together). The Windows
# sibling recording-verdict-on-stream.sh (win-* MCP / PowerShell, C:\camera-box paths) cannot reach a
# Linux box.
#
# This script is the stream box's ENTRY POINT onto the ONE Linux on-box decode,
# scripts/recording-verdict-on-strih-snv.sh (plain ssh/scp, the issue-1118 sha256 upload gate of the
# verdict binary, --skip-if-exists, the decode at idle priority pinned to the box's own E-cores, the
# partial JSON + its <partial>-pixels dir pulled back unchanged). It adds only what is stream-specific:
#   * the target = STREAM_LX_BOX (default: the obs-fleet host of stream-snv, never a literal address),
#     with STREAM_USER / STREAM_PW (default newlevel / newlevel, targets.md);
#   * a box whose obs-fleet class is not linux-genlock is REFUSED (exit 2) before any contact -- a
#     Windows stream box decodes through recording-verdict-on-stream.sh;
#   * `--stream-rec <path>` (the StopRecord path on the box) becomes the decode's `--rec`;
#   * the log tag is the box's fleet name (ONBOX_LABEL).
# Every other argument (--verdict-bin, --remote-bin, --out-dir, --local-out-dir, --skip-if-exists, and
# everything after `--`, which goes to recording-verdict verbatim) passes through unchanged.
#
# Usage (recording-e2e.sh run_stream_extract on a Linux stream box):
#   STREAM_LX_BOX=10.77.9.204 recording-verdict-on-stream-snv.sh \
#       --verdict-bin <dev1 probe-tools recording-verdict> --out-dir /home/newlevel/verdict-out \
#       --local-out-dir <run dir> --stream-rec '/home/newlevel/2026-10-24 02-58-45.mkv' \
#       -- --extract-partial stream --stream '/home/newlevel/2026-10-24 02-58-45.mkv' ... \
#          --out /home/newlevel/verdict-out/stream-partial-<RUN_ID>.json
# Exit: the decode's (0 = the partial is on dev1); 2 = refused (no box / not Linux / no recording).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/stream-box.sh
. "$HERE/lib/stream-box.sh"

main() {
  local box="${STREAM_LX_BOX:-}" label rec="" have_rec=0
  [ -n "$box" ] || box="$(obs_fleet_host stream-snv 2>/dev/null || true)"
  if [ -z "$box" ]; then
    echo "ERROR: [8/8b] no stream box to decode on -- STREAM_LX_BOX is unset and the obs-fleet list has no stream-snv row (issue 1462)" >&2
    exit 2
  fi
  if [ "$(stream_box_platform "$box")" != "linux" ]; then
    echo "ERROR: [8/8b] ${box} is not a Linux stream box (its obs-fleet class is not linux-genlock) -- a Windows stream box decodes through recording-verdict-on-stream.sh (issue 1462)" >&2
    exit 2
  fi
  label="$(obs_fleet_name_for_host "$box" 2>/dev/null || true)"
  [ -n "$label" ] || label="stream-snv"

  local -a args=()
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --stream-rec) rec="${2:-}"; have_rec=1; args+=(--rec "${2:-}"); shift 2 ;;
      --)           args+=("$@"); break ;;
      *)            args+=("$1"); shift ;;
    esac
  done
  if [ "$have_rec" -eq 1 ] && [ -z "$rec" ]; then
    echo "ERROR: [8/8b] --stream-rec is empty -- the stream box's StopRecord returned no recording path, so the stream leg (and its A/V measurement) cannot be decoded (issue 1462)" >&2
    exit 2
  fi

  STRIH_LX_BOX="$box" STRIH_USER="${STREAM_USER:-newlevel}" STRIH_PW="${STREAM_PW:-newlevel}" ONBOX_LABEL="$label" \
    exec "$HERE/recording-verdict-on-strih-snv.sh" "${args[@]}"
}

main "$@"
