#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects), mirrors the sibling
# scripts/lib/audio-presence-preflight.sh convention.
#
# scripts/lib/marker-decodability-preflight.sh — pre-record QPSK-marker DECODABILITY preflight
# (#1324). The honest sibling of the #1323 level ceiling in audio-presence-preflight.sh.
#
# #1324 root cause (live 16.9.2026): the #748 floor proves NOT-silent and the #1323 ceiling proves
# NOT-flooded, but NEITHER can tell a DECODABLE QPSK marker from a chain at a plausible level whose
# marker is not decodable (drowned, off-axis mic, wrong Dante channel, format mismatch). The failed
# runs 622403283 / 977889848 read a plausible level, yet EVERY cam's cluster_samples=0 — and the
# demod actually decoded MORE markers than the green run (480/551 false CRC-passes vs 107 real),
# so a raw decode count does NOT separate good from bad. What separates them is SELF-CONSISTENCY:
# real markers arrive on ONE emit cadence, false decodes scatter. The run then burns ~40 min before
# the A/V-offset gate fails on cluster_samples=0.
#
# This preflight captures ~25 s of the stream mbc track (the SAME probe-recording hop the #748
# audio-presence step uses), extracts the mbc audio track to a small mono-f32 WAV on the stream box
# via ffmpeg, pulls it to dev1, and runs the AUDIO-ONLY QPSK decodability probe from the
# probe-tools artifact ($PROBE_BIN_DIR/recording-verdict --qpsk-probe). The probe reuses the SAME
# demod as --av-sync (no emit-log/video pairing) and prints ONE JSON line
# {preamble_screens,candidates,cluster_samples,crc_ok,crc_fail,peak_dbfs,verdict}. This lib holds
# the PURE decision logic (thresholds, the remote command builders, the JSON field parse, the
# class-named messages) so it is Tier-0 unit-testable; the recording-e2e.sh [4b3/8] step is a thin
# caller. Decodability is PRIMARY (a loud-but-decodable capture is OK); the #1323 −20 bar is a
# COVARIATE the probe applies internally, never a standalone reject here.
#
# Source-only: this file defines pure functions and performs no side effects on its own.

# marker_decodability_default_min_clusters -> the canonical minimum self-consistency cluster size
# (4). Calibrated on the real 16.9 recordings: a healthy 25 s window clusters >= 7, a drowned one
# <= 3, so 4 leaves margin on BOTH sides (never false-fail a good run, still catch the bad one).
# This is the ONE source of the 4 literal — the [4b3/8] step's default-arg site references it, and
# the same value is the Rust probe's --qpsk-min-clusters default (kept in lock-step by the tests).
marker_decodability_default_min_clusters() {
  printf '%s\n' "4"
}

# marker_decodability_default_probe_secs -> the capture/analysis window (25 s). ~25 s spans ~8
# emitter markers (~3 s cadence) so a healthy chain clusters well above the floor; the ONE source
# of the 25 literal.
marker_decodability_default_probe_secs() {
  printf '%s\n' "25"
}

# marker_decodability_extract_wav_ps REC_WIN WAV_WIN TRACK -> the PowerShell command text (for
# win_ssh_run) that extracts audio TRACK of the probe recording REC_WIN to a mono f32 @ 48 kHz WAV
# at WAV_WIN on the stream box. Mono mix + 48 kHz match what the probe re-reads (a passthrough
# there); pcm_f32le keeps the marker amplitude intact. `-y` overwrites a stale WAV; stderr merged so
# a failure surfaces in the captured output.
marker_decodability_extract_wav_ps() {
  local rec="$1" wav="$2" track="$3"
  printf 'ffmpeg -hide_banner -nostats -y -i "%s" -map 0:a:%s -ac 1 -ar 48000 -c:a pcm_f32le "%s" 2>&1' \
    "$rec" "$track" "$wav"
}

# marker_decodability_delete_ps WIN_PATH -> best-effort delete of the throwaway WAV on the stream
# box (SilentlyContinue — a failed delete leaves a small orphan, never aborts the run).
marker_decodability_delete_ps() {
  printf 'Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath "%s"' "$1"
}

# marker_decodability_parse_num JSON KEY -> the numeric value of KEY in the one-line probe JSON
# (integer or signed decimal, e.g. cluster_samples / preamble_screens / peak_dbfs). Empty + non-zero
# exit when absent (an unparseable probe output is NEVER treated as a pass — the caller routes an
# empty parse to the unreadable diagnostic, mirroring audio_preflight_parse_max_db).
marker_decodability_parse_num() {
  local json="$1" key="$2" v
  v="$(printf '%s' "$json" \
       | grep -aoE "\"${key}\":[ ]*-?[0-9]+(\.[0-9]+)?" \
       | head -1 \
       | grep -aoE '\-?[0-9]+(\.[0-9]+)?' \
       | head -1)"
  [ -z "$v" ] && return 1
  printf '%s\n' "$v"
}

# marker_decodability_parse_verdict JSON -> the verdict word (OK/UNDECODED/SILENT/POLLUTED). Empty +
# non-zero exit when absent.
marker_decodability_parse_verdict() {
  local json="$1" v
  v="$(printf '%s' "$json" \
       | grep -aoE '"verdict":[ ]*"[A-Z]+"' \
       | head -1 \
       | grep -aoE '[A-Z]+' \
       | head -1)"
  [ -z "$v" ] && return 1
  printf '%s\n' "$v"
}

# marker_decodability_is_ok VERDICT -> "true"/"false". Only the literal OK verdict proceeds; every
# other word (UNDECODED/SILENT/POLLUTED) is a fail that names its class in the message below.
marker_decodability_is_ok() {
  [ "$1" = "OK" ] && echo "true" || echo "false"
}

# marker_decodability_fail_message VERDICT CLUSTERS MIN PREAMBLE PEAK -> the operator-facing abort
# message, a pure string formatter (no I/O) so it is directly unit-testable. Names the verdict
# CLASS, the measured cluster size vs the floor, the preamble-screen count, and the level — plus the
# class-specific cause to check. The verdict words are disjoint so the class is never ambiguous.
marker_decodability_fail_message() {
  local verdict="$1" clusters="$2" min="$3" preamble="$4" peak="$5" cause
  case "$verdict" in
    SILENT)
      cause="the marker is not even present at level (peak ${peak} dBFS is below the −60 dB silence floor) — check the mbc Ableton mic channel is UNMUTED and the Dante routing into stream OBS (this is the #748 class, drifted in after the audio-presence step)." ;;
    POLLUTED)
      cause="a LOUD foreign signal (peak ${peak} dBFS) is drowning the marker while it stays undecodable — quiet whatever is routed hot into the measurement mic / mbc Ableton channel / Dante back to the marker-only level (the #1323 class)." ;;
    *)
      cause="the marker is present at a plausible level (peak ${peak} dBFS) but the demod cannot decode a self-consistent cadence — the mbc chain mangles it (mic moved off-axis / a wrong Dante channel carrying speech / a sample-rate or format mismatch). Fix the speaker→mic→mbc→Dante chain and confirm the cam2 QPSK marker is audible + clean at the mic." ;;
  esac
  echo "measurement audio ${verdict} — the mbc QPSK marker is NOT DECODABLE (self-consistency cluster ${clusters} < ${min}, preamble_screens ${preamble}). ${cause} The A/V-sync leg reads this marker, so a run on an undecodable measurement instrument burns a full ~40-min cycle before the A/V-offset gate fails on cluster_samples=0 (#1324)."
}

# marker_decodability_unverified_message -> the loud UNVERIFIED note when the probe-tools binary is
# absent (the ONE sanctioned skip — never a silent pass). $PROBE_BIN_DIR/recording-verdict is the
# same artifact the E2E already downloads; if it is missing the decodability leg cannot run.
marker_decodability_unverified_message() {
  echo "UNVERIFIED: the marker-decodability preflight was SKIPPED — \$PROBE_BIN_DIR/recording-verdict (the probe-tools artifact) is absent, so the QPSK decodability of the mbc chain could not be checked before recording (#1324). Download the probe-tools CI artifact to enable it."
}

# marker_decodability_probe_unreadable_message RAW -> the fail message when the probe binary is
# present but produced no parseable JSON verdict (ffmpeg missing on dev1, the WAV never arrived, a
# probe crash) — distinct from "readable but UNDECODED", never conflated (mirrors
# audio_preflight_unreadable_message).
marker_decodability_probe_unreadable_message() {
  echo "measurement-audio decodability preflight could not parse a verdict JSON from the probe — cannot confirm the mbc QPSK marker is decodable before recording. Check ffmpeg is on PATH on dev1 and that the mbc WAV was captured + pulled back (#1324). raw: $1"
}
