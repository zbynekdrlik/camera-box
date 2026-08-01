#!/usr/bin/env bash
# lipsync-asset.sh -- issue 930: fetch + verify + trim the lipsync cross-validation test asset,
# reproducibly, from the pinned source in assets/lipsync/PROVENANCE.md (never committed to git --
# see that file for the license/provenance/trim-recipe details this script implements).
#
set -euo pipefail
#
# Usage:
#   lipsync-asset.sh fetch      -- download the pinned source, verify its sha256, sample a frame,
#                                  trim to the working test.mp4 (assets/lipsync/test.mp4)
#   lipsync-asset.sh baseline   -- run av_sync_measure.py --media on the ALREADY-fetched test.mp4
#                                  to confirm its intrinsic A/V offset before trusting it as a
#                                  cross-check reference (per PROVENANCE.md's own convention)
#
# Env:
#   LIPSYNC_ASSET_DIR   (default: <repo>/assets/lipsync) -- where source.ogv/test.mp4 land.
#   LIPSYNC_REPO_ROOT   (default: syncnet_python checkout on the STREAM box, C:\avsync\syncnet_python
#                        translated for `baseline` -- ONLY used when running `baseline` ON the
#                        stream box itself; a dev1 invocation of `baseline` needs SyncNet reachable
#                        on $PATH, which dev1 does not have -- see .claude/skills/av-sync for the
#                        SyncNet install location and the standalone-run stub recipe).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/.." && pwd)"

# --------------------------------------------------------------------------------------------- #
# PURE functions (no network, no filesystem writes) -- sourced + unit-tested by
# tests/harness_lipsync_asset.rs without ever hitting the network.
# --------------------------------------------------------------------------------------------- #

# lipsync_asset_source_url -- the pinned Wikimedia Commons direct-media URL (PROVENANCE.md).
lipsync_asset_source_url() {
  echo 'https://upload.wikimedia.org/wikipedia/commons/4/45/Kamala_Harris%27_speech_during_Celebrating_America.ogv'
}

# lipsync_asset_source_sha256 -- the pinned sha256 of the WHOLE source file (PROVENANCE.md).
lipsync_asset_source_sha256() {
  echo '7ece8fe0ae7aba1374ca9951c0a8f0ca5a9816430d95a38880f93ef87c533b78'
}

# lipsync_asset_trim_cmd <src> <out> -- the exact, deterministic ffmpeg trim command
# (PROVENANCE.md's own recipe: 30s..90s, 1280x720@60fps H.264/AAC 44.1kHz stereo). Pure string
# builder, no execution -- callers `eval`/run it, tests just inspect the printed argv.
lipsync_asset_trim_cmd() {
  local src="$1" out="$2"
  printf 'ffmpeg -y -ss 30 -i %q -t 60 -vf scale=1280:720 -r 60 -c:v libx264 -pix_fmt yuv420p -c:a aac -ar 44100 -ac 2 %q' \
    "$src" "$out"
}

# lipsync_asset_baseline_cmd <python> <av_sync_measure.py> <media> [calibration_log] -- the
# av_sync_measure.py invocation that measures the TRIMMED asset's own intrinsic A/V offset
# (no --grab, no OBS connection -- a plain --media one-shot). Pure string builder.
lipsync_asset_baseline_cmd() {
  local py="$1" script="$2" media="$3" calibration_log="${4:-}"
  local cmd
  cmd="$(printf '%q' "$py") $(printf '%q' "$script") --media $(printf '%q' "$media")"
  if [ -n "$calibration_log" ]; then
    cmd="$cmd --calibration-log $(printf '%q' "$calibration_log")"
  fi
  echo "$cmd"
}

# lipsync_asset_verify_sha256 <file> <expected_sha256> -- true (0) iff <file>'s sha256 matches.
lipsync_asset_verify_sha256() {
  local file="$1" expected="$2" actual
  actual="$(sha256sum "$file" | awk '{print $1}')"
  [ "$actual" = "$expected" ]
}

# --------------------------------------------------------------------------------------------- #
# Subcommands
# --------------------------------------------------------------------------------------------- #

cmd_fetch() {
  local dir="${LIPSYNC_ASSET_DIR:-$REPO_ROOT/assets/lipsync}"
  mkdir -p "$dir"
  local src="$dir/source.ogv"
  local out="$dir/test.mp4"
  local expected
  expected="$(lipsync_asset_source_sha256)"

  if [ -f "$src" ] && lipsync_asset_verify_sha256 "$src" "$expected"; then
    echo "[lipsync-asset] $src already present + sha256 verified -- skipping download"
  else
    echo "[lipsync-asset] fetching $(lipsync_asset_source_url) -> $src"
    curl -sL --fail -o "$src" "$(lipsync_asset_source_url)"
    lipsync_asset_verify_sha256 "$src" "$expected" || {
      echo "[lipsync-asset] FAIL: sha256 mismatch on $src (expected $expected, got $(sha256sum "$src" | awk '{print $1}'))" >&2
      echo "[lipsync-asset] the pinned source in assets/lipsync/PROVENANCE.md may be stale, or the download was corrupted -- NOT trusting this file" >&2
      rm -f "$src"
      exit 1
    }
    echo "[lipsync-asset] sha256 verified: $expected"
  fi

  echo "[lipsync-asset] sampling a frame for a by-eye sanity check (well-lit single face expected)"
  ffmpeg -y -ss 45 -i "$src" -frames:v 1 -q:v 3 "$dir/sample-frame.jpg" -loglevel error

  echo "[lipsync-asset] trimming -> $out"
  eval "$(lipsync_asset_trim_cmd "$src" "$out")" -loglevel error
  echo "[lipsync-asset] done: $out ($(du -h "$out" | cut -f1))"
}

cmd_baseline() {
  local dir="${LIPSYNC_ASSET_DIR:-$REPO_ROOT/assets/lipsync}"
  local media="$dir/test.mp4"
  [ -f "$media" ] || {
    echo "[lipsync-asset] FAIL: $media not found -- run 'lipsync-asset.sh fetch' first" >&2
    exit 1
  }
  local py="${LIPSYNC_PYTHON:-python3}"
  local script="${LIPSYNC_AV_SYNC_MEASURE:-$REPO_ROOT/scripts/av_sync_measure.py}"
  echo "[lipsync-asset] baseline: measuring $media's OWN intrinsic A/V offset (before trusting it as a cross-check reference)"
  eval "$(lipsync_asset_baseline_cmd "$py" "$script" "$media")"
}

main() {
  case "${1:-}" in
    fetch) cmd_fetch ;;
    baseline) cmd_baseline ;;
    *)
      echo "usage: $0 {fetch|baseline}" >&2
      exit 2
      ;;
  esac
}

# Run main only when EXECUTED, not when SOURCED (tests/harness_lipsync_asset.rs sources this
# file and calls the pure functions directly without triggering a real fetch).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
