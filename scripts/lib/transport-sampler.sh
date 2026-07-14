#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no ssh / device I/O at source time), mirrors
# the sibling scripts/lib/tmp-burn-sweep.sh / camera-box-restart-verify.sh convention: every
# function RETURNS a REMOTE COMMAND STRING the caller embeds via command substitution into its OWN
# `sshpass ... ssh root@<box> "$(...)"` call. This file itself performs no ssh, no device I/O, and
# deliberately does NOT `set -euo pipefail` (it is sourced into recording-e2e.sh, which owns its own
# shell options). Pure functions -> unit-testable off-rig (tests/harness_transport_sampler_707.rs).
#
# scripts/lib/transport-sampler.sh — #707 B1 (freeze+jump discriminator, second prong).
#
# WHY: #707's 2026-07-14 CORRECTION split the residual into two named mechanisms. B1 (FREEZE+JUMP)
# is a genuine ~2.7s delivery interruption on a cam box -> strih NDI path where the missing frames
# NEVER ARRIVE at strih (received shows no catch-up burst) — but every box-side instrument
# (capture_stall / send_stall / v4l2_dropped) reads clean, so the final layer (link vs box-emit vs
# NDI SDK internal) is unnamed. The supervisor's 2026-07-14 deep-analysis predicts a TRANSPORT
# (TCP) stall on that box's connection to strih: a ballooning Send-Q and/or retransmit spikes, or
# NIC error/pause counter increments, concurrent with a residual window. This sampler records those
# signals during the recording window so the next freeze's discriminator can be READ, not guessed:
#   - Send-Q balloons / retransmits spike  -> LINK layer (cable / switch port / NIC) — #688 list
#   - box-side 1s emit dips (emit_rate_ring, prong 1) instead -> box emit path — file on #707
#   - all clean while strih freezes         -> NDI SDK internal drop — document honestly
#
# It samples, on EACH cam box, every ~250ms during [5/8]->[7/8]: the box's TCP connection(s) to
# strih (`ss -tn`/`ss -tin dst <strih>` — summed Send-Q + total retransmits) and the egress NIC's
# cumulative RX/TX error+drop counters (`ip -s link show <iface>`), one CSV row per sample into a
# per-run CSV on the box, scp'd back to the run dir (harvested with the artifacts). ABSOLUTE
# counters per row (the analysis computes deltas) — simpler + more robust than in-bash deltas.

# The CSV column contract — single source of truth (the loop body writes this header; a consumer
# asserts it). ABSOLUTE cumulative counters; rx/tx are the egress iface's error,dropped pair.
transport_sampler_csv_header() {
	printf 'epoch_ms,iface,send_q_bytes,retrans_total,rx_errors,rx_dropped,tx_errors,tx_dropped\n'
}

# The remote sampling LOOP body, emitted literally (a quoted heredoc: awk single-quotes, `$3`,
# `$now` etc. all pass through untouched). It reads its parameters from the exported env vars
# TS_STRIH / TS_CSV / TS_MAX / TS_PID (set by transport_sampler_remote_start_cmd below), so the
# body needs no interpolation at all — no nested-quoting footgun (the e2e skill's rig-mode heredoc
# gotcha). Self-terminates after TS_MAX seconds OR when a `<TS_PID>.stop` sentinel appears, so an
# un-stopped sampler can never orphan on the box (the box's /tmp is tmpfs; it also self-cleans on
# reboot).
transport_sampler_loop_body() {
	cat <<'TSBODY'
set +e
end=$(( $(date +%s) + ${TS_MAX:-600} ))
iface=$(ip route get "$TS_STRIH" 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p' | head -1)
[ -z "$iface" ] && iface=unknown
printf 'epoch_ms,iface,send_q_bytes,retrans_total,rx_errors,rx_dropped,tx_errors,tx_dropped\n' > "$TS_CSV"
while [ "$(date +%s)" -lt "$end" ] && [ ! -f "${TS_PID}.stop" ]; do
  now=$(date +%s%3N)
  sq=$(ss -tn "dst $TS_STRIH" 2>/dev/null | awk 'NR>1{s+=$3} END{print s+0}')
  rtx=$(ss -tin "dst $TS_STRIH" 2>/dev/null | grep -oE 'retrans:[0-9]+/[0-9]+' | awk -F/ '{s+=$2} END{print s+0}')
  st=$(ip -s link show "$iface" 2>/dev/null)
  rx=$(printf '%s\n' "$st" | awk '/RX:/{getline; print $3","$4; exit}')
  tx=$(printf '%s\n' "$st" | awk '/TX:/{getline; print $3","$4; exit}')
  printf '%s,%s,%s,%s,%s,%s\n' "$now" "$iface" "${sq:-0}" "${rtx:-0}" "${rx:-0,0}" "${tx:-0,0}" >> "$TS_CSV"
  sleep 0.25
done
TSBODY
}

# Echo the remote command that ARMS the sampler on one box (backgrounded). The caller runs it as
# `sshpass ... ssh root@<box> "$(transport_sampler_remote_start_cmd <strih> <interval_ms> <csv> <max_secs> <pidfile>)"`.
# The loop body is %q-quoted so the box's bash re-parses it verbatim (awk single-quotes survive);
# params are %q-quoted env assignments. Prints `transport-sampler-armed` on success.
# (interval is currently fixed at 250ms inside the loop body — the arg is accepted for a future
# override and to keep the call site self-documenting; kept unused here on purpose.)
transport_sampler_remote_start_cmd() {
	local strih="$1" csv="$3" max="$4" pidfile="$5"
	local body_q
	body_q=$(printf '%q' "$(transport_sampler_loop_body)")
	printf 'export TS_STRIH=%q TS_CSV=%q TS_MAX=%q TS_PID=%q; rm -f "${TS_PID}.stop" 2>/dev/null; nohup bash -c %s >/dev/null 2>&1 & echo $! > "$TS_PID"; echo transport-sampler-armed' \
		"$strih" "$csv" "$max" "$pidfile" "$body_q"
}

# Echo the remote command that STOPS the sampler (sentinel first so the loop exits cleanly, then a
# hard kill of the recorded PID as a backstop) and removes the pidfile + sentinel.
transport_sampler_remote_stop_cmd() {
	local pidfile="$1"
	printf 'touch %q.stop 2>/dev/null; sleep 0.4; [ -f %q ] && kill "$(cat %q)" 2>/dev/null; rm -f %q %q.stop 2>/dev/null; true' \
		"$pidfile" "$pidfile" "$pidfile" "$pidfile" "$pidfile"
}

# Map a cam box IP to a stable short name for the harvested CSV filename (last octet -> cam name).
transport_sampler_box_label() {
	case "${1##*.}" in
	61) echo cam1 ;;
	62) echo cam2 ;;
	63) echo cam3 ;;
	64) echo cam4 ;;
	65) echo cam5 ;;
	66) echo cam6 ;;
	*) echo "box-${1##*.}" ;;
	esac
}
