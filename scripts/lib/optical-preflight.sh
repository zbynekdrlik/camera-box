#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- matches
# the sibling scripts/lib/*.sh convention (capture-rate-guard.sh, optical-chain-preflight.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the CALLER's
# shell, so imposing strict mode here would leak into whichever caller sources it. recording-e2e.sh
# (the only caller) already sets -euo pipefail itself.
#
# scripts/lib/optical-preflight.sh -- #1141: the recording-e2e.sh [0/8] head-end OPTICAL blur/
# shutter fail-fast. The #675 prevention pattern: a NEW sourced-lib function invoked with ONE line
# from recording-e2e.sh, so NO existing static-anchor line in that file is edited.
#
# WHY (#1141): a genuinely misconfigured camera (slow shutter 1/60, PAL/50 Hz, anti-flicker) blurs
# the captured picture, which the existing [0/8] capture-RATE preflight (#656) is BLIND to (a 1/60
# shutter still captures ~60 frames/s, just smeared). That is the #216 precedent (1/60 shutter →
# 16.7 ms exposure = a full 60 Hz frame period → the moving dual-QR smears → optically undecodable →
# a 175 s optical-read gap). This preflight reads the head-end `rough=` capture telemetry the
# running camera-box service ALREADY logs (src/capture.rs luma_roughness, #1079: mean |Y0−Y1| of
# adjacent luma pairs — HIGH for a crisp pattern, LOW when blur smears them) and aborts LOUD when it
# is SUSTAINED at/below the calibrated floor. No decode tooling, no service stop (verified live
# 2026-08-20: the cam boxes have no zbar and no probe binary), immune to the imag x264 OBSERVER
# EFFECT (#1130) because it reads the capture chain BEFORE the recorder.
#
# DECISION PARITY: the pure crate-root src/optical_preflight.rs is the SOURCE OF TRUTH for the
# thresholds + the classify decision + the Slovak abort message; the three const/message functions
# and the classify awk below REPLICATE it, and tests/harness_optical_preflight_1141.rs pins the two
# together so they can never drift (the repo's python/shell anchor-replication pattern).

# optical_preflight_rough_floor -> the head-end roughness ABORT floor (MUST equal
# OPTICAL_PREFLIGHT_ROUGH_FLOOR in src/optical_preflight.rs). A sustained median at/below this = a
# blurred, misconfigured camera.
optical_preflight_rough_floor() { echo '2.5'; }

# optical_preflight_min_samples -> the minimum finite rough= sample count before JUDGING (MUST equal
# OPTICAL_PREFLIGHT_MIN_SAMPLES in src/optical_preflight.rs). Fewer = INSUFFICIENT -> NOTE + proceed.
optical_preflight_min_samples() { echo '5'; }

# optical_preflight_abort_message -> the operator-facing NAMED abort message (Slovak). BYTE-for-byte
# equal to OPTICAL_PREFLIGHT_ABORT_MESSAGE in src/optical_preflight.rs (pinned by the harness test).
optical_preflight_abort_message() {
  printf '%s' 'kamera je zle nastavená — snímaný obraz je rozmazaný (pomalý shutter / anti-flicker), dual-QR sa opticky nedá čítať. Nastav shutter 1/500+, 60p, anti-flicker/flicker OFF. Treba FYZICKY nastaviť kameru — softvér to nevyrieši.'
}

# optical_preflight_journalctl_cmd INVOCATION_ID [LINES] -> the REMOTE journalctl command text that
# reads ONLY the CURRENT camera-box.service process instance's log lines (via _SYSTEMD_INVOCATION_ID,
# the #693 freshness scoping the capture-rate preflight established) so a `rough=` line from a killed
# prior instance can never leak into the window. Falls back to the unscoped `-u camera-box` form when
# INVOCATION_ID is empty (systemctl show failed). Pure string builder (no ssh) -> directly testable.
optical_preflight_journalctl_cmd() {
  local invocation_id="${1:-}" lines="${2:-300}"
  if [ -n "$invocation_id" ]; then
    printf 'journalctl _SYSTEMD_INVOCATION_ID=%s --no-pager -n %s 2>/dev/null' "$invocation_id" "$lines"
  else
    printf 'journalctl -u camera-box --no-pager -n %s 2>/dev/null' "$lines"
  fi
}

# optical_preflight_classify < journal-text -> the pure decision, printed on ONE line:
#   "INSUFFICIENT"            (fewer than min finite rough= samples -> NOTE + proceed)
#   "SICK_BLUR <median>"      (median at/below the floor -> ABORT)
#   "HEALTHY <median>"        (median above the floor -> proceed)
# Extracts ONLY `rough=<number>` tokens (mirrors src/optical_preflight.rs::parse_rough_samples — a
# bare number like the "16.0" in "NDI display: 16.0 fps" is NOT counted) and uses the MEDIAN (not the
# mean or a single dip) as the "sustained" test: a lone spurious low sample can never cross the floor,
# so a healthy run is never false-aborted (the owner's hardest constraint). READS STDIN.
optical_preflight_classify() {
  local floor min
  floor="$(optical_preflight_rough_floor)"
  min="$(optical_preflight_min_samples)"
  # LC_ALL=C: force '.'-decimal parsing — the rough= journal values, the floor, and the median
  # are all '.'-decimal, but mawk/gawk string->number conversion is locale-sensitive (strtod), so a
  # comma-locale box could mis-parse "2.5"/"7.6" and shift the floor comparison. Locale-hardened like
  # the repo's own grep call sites.
  LC_ALL=C awk -v floor="$floor" -v min="$min" '
    {
      for (i = 1; i <= NF; i++) {
        tok = $i
        if (tok ~ /^rough=[0-9]+(\.[0-9]+)?$/) {
          sub(/^rough=/, "", tok)
          v[n++] = tok + 0
        }
      }
    }
    END {
      if (n < min) { print "INSUFFICIENT"; exit }
      for (i = 0; i < n; i++)
        for (j = i + 1; j < n; j++)
          if (v[j] < v[i]) { t = v[i]; v[i] = v[j]; v[j] = t }
      if (n % 2 == 1) med = v[int(n / 2)]
      else med = (v[n / 2 - 1] + v[n / 2]) / 2
      if (med <= floor) printf "SICK_BLUR %.2f\n", med
      else printf "HEALTHY %.2f\n", med
    }
  '
}

# optical_preflight_assert CAM_IP CAM_USER CAM_PW CAM_NAME
#   Reads the source box's recent head-end `rough=` capture telemetry over ONE ssh, classifies it,
#   and:
#     - EXITS 1 (loud, NAMED) when the median roughness is SUSTAINED at/below the floor (a blurred,
#       misconfigured camera — the abortable incident),
#     - NOTEs + proceeds on INSUFFICIENT data or an unreachable box (never abort on thin telemetry /
#       ssh hiccup — the fleet-reachability gate owns genuine unreachability),
#     - reports ok otherwise.
#   Call it as a PLAIN statement (never in a pipeline/$()) so its `exit 1` propagates to the harness.
optical_preflight_assert() {
  local cam_ip="$1" cam_user="$2" cam_pw="$3" cam_name="$4"
  local invocation_id journal verdict state median

  invocation_id="$(timeout 15 sshpass -p "$cam_pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${cam_user}@${cam_ip}" "systemctl show -p InvocationID --value camera-box 2>/dev/null" 2>/dev/null || true)"

  journal="$(timeout 20 sshpass -p "$cam_pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${cam_user}@${cam_ip}" "$(optical_preflight_journalctl_cmd "$invocation_id") | grep -F 'capture chroma:'" 2>/dev/null || true)"

  if [ -z "$journal" ]; then
    echo "    NOTE: [0/8] optical head-end preflight — no head-end rough= telemetry read from $cam_name over ssh ($cam_ip); the fleet reachability gate owns that condition — skipping the blur fail-fast" >&2
    return 0
  fi

  verdict="$(printf '%s\n' "$journal" | optical_preflight_classify)"
  state="${verdict%% *}"
  median="${verdict#* }"

  case "$state" in
    HEALTHY)
      echo "    ok: [0/8] optical head-end preflight — $cam_name capture is crisp (median rough=$median > podlaha $(optical_preflight_rough_floor))" ;;
    INSUFFICIENT)
      echo "    NOTE: [0/8] optical head-end preflight — $cam_name has too few recent rough= samples to judge (service just restarted?); proceeding — the recording verdict still measures the optical hop" >&2 ;;
    SICK_BLUR)
      echo "ERROR: [0/8] optický head-end preflight: $cam_name $(optical_preflight_abort_message) (median rough=$median ≤ podlaha $(optical_preflight_rough_floor))" >&2
      echo "       Kamera sníma v správnom RATE (preto #656 preflight prešiel), ale ROZMAZANE — pomalý shutter / anti-flicker (presne #216 precedens: 175 s optical-read gap). Softvér to nevyrieši; treba FYZICKY prenastaviť kameru, potom re-run." >&2
      exit 1 ;;
    *)
      echo "    NOTE: [0/8] optical head-end preflight — unexpected classifier output for $cam_name ('$verdict'); proceeding rather than false-aborting a CI gate" >&2 ;;
  esac
  return 0
}
