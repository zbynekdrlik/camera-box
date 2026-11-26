#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, which sets its own strict mode.
#
# scripts/lib/rec-pull-paced.sh -- issue 1425 (owner ROZHODNUTÉ issuecomment-7187562277, 26.11.2026):
# the full-path E2E's recording decode + recording-verdict run on dev1, NEVER on a rig box. A rig
# recording reaches dev1 through this ONE function, used by the [8/8a]/[8/8b] decode
# (recording-verdict-on-strih-snv.sh, which the stream box execs) and by the missing-slot pixel proofs
# (missing-slot-pixels.sh):
#
#   rec_pull_paced USER PW HOST REMOTE_PATH LOCAL_PATH [LABEL] [LOWPRIO_SNIPPET]
#     1. on the box: the size (`stat -c %s`) and the sha256 of REMOTE_PATH, the sha256sum run behind
#        the caller's idle-priority prefix (LOWPRIO_SNIPPET sets $LP; default `nice -n 19`). That
#        read is the ONLY process this starts on the box besides scp's own file server;
#     2. scp PACED with `-l` to REC_PULL_RATE_MBIT (default 200 Mbit/s = 25 MB/s): a ~790 MB
#        6-min E2E recording takes ~32 s, both boxes in parallel 400 Mbit/s into dev1 -- the rig NIC
#        never bursts at line rate. Bounded by REC_PULL_TIMEOUT (default 1800 s);
#     3. on dev1: size + sha256 of LOCAL_PATH against the box's, and one log line with bytes,
#        seconds, the achieved rate, the cap and the sha match.
#   rc 0 = LOCAL_PATH holds a verified byte-identical copy. rc 1 = anything failed (a named
#   `[rec-pull] ERROR:` line on stderr) and LOCAL_PATH is removed: never a partial copy left behind.
#   The caller deletes LOCAL_PATH when its decode is done (the ROZHODNUTÉ: no store of recordings).

# rec_pull_rate_mbit -> the pacing cap in Mbit/s: REC_PULL_RATE_MBIT when it is a positive whole
# number (ASCII digits, never a locale range), else 200. Pure.
rec_pull_rate_mbit() {
  local v="${REC_PULL_RATE_MBIT:-}"
  if [[ $v =~ ^[123456789][0123456789]{0,5}$ ]]; then printf '%s' "$v"; else printf '200'; fi
}

# rec_pull_box_stat_cmd REMOTE_PATH [LOWPRIO_SNIPPET] -> the remote bash text that prints two lines:
# the file size in bytes, then its sha256 (hex only). The path is %q-quoted (OBS names carry a space).
# Pure.
rec_pull_box_stat_cmd() {
  local snippet="${2:-}"
  [ -n "$snippet" ] || snippet='LP="nice -n 19";'
  # shellcheck disable=SC2016  # $LP and $F are expanded by the REMOTE shell, on purpose
  printf 'F=%q; %s stat -c %%s -- "$F" && $LP sha256sum -- "$F" | cut -d" " -f1' "$1" "$snippet"
}

rec_pull_paced() {
  local user="$1" pw="$2" host="$3" rpath="$4" lpath="$5" label="${6:-rec}" snippet="${7:-}"
  local -a ssh_opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10)
  local tmo="${REC_PULL_TIMEOUT:-1800}" rate stat box_size box_sha t0 t1 loc_size loc_sha
  [[ $tmo =~ ^[123456789][0123456789]*$ ]] || tmo=1800
  rate="$(rec_pull_rate_mbit)"
  if ! stat="$(timeout --foreground -k 5 600 sshpass -p "$pw" ssh "${ssh_opts[@]}" "$user@$host" "$(rec_pull_box_stat_cmd "$rpath" "$snippet")" 2>/dev/null)"; then
    echo "[rec-pull] ERROR: $label: could not read the size + sha256 of $host:$rpath" >&2
    return 1
  fi
  box_size="$(printf '%s\n' "$stat" | sed -n 1p | tr -dc '0123456789')"
  box_sha="$(printf '%s\n' "$stat" | sed -n 2p | tr -dc '0123456789abcdef')"
  if [ -z "$box_size" ] || [ "${#box_sha}" -ne 64 ]; then
    echo "[rec-pull] ERROR: $label: unreadable size/sha256 of $host:$rpath (got '${stat//$'\n'/ | }')" >&2
    return 1
  fi
  mkdir -p "$(dirname "$lpath")"
  echo "[rec-pull] $label: copying $host:$rpath ($box_size B) -> $lpath paced at $rate Mbit/s"
  t0="$(date +%s.%N)"
  if ! timeout --foreground -k 5 "$tmo" sshpass -p "$pw" scp -q -l "$((rate * 1000))" "${ssh_opts[@]}" "$user@$host:$rpath" "$lpath" 2>/dev/null; then
    rm -f -- "$lpath"
    echo "[rec-pull] ERROR: $label: the copy of $host:$rpath failed or timed out (${tmo}s)" >&2
    return 1
  fi
  t1="$(date +%s.%N)"
  loc_size="$(stat -c %s -- "$lpath" 2>/dev/null || true)"
  loc_sha="$(sha256sum -- "$lpath" 2>/dev/null | cut -d' ' -f1 || true)"
  if [ "$loc_size" != "$box_size" ] || [ "$loc_sha" != "$box_sha" ]; then
    rm -f -- "$lpath"
    echo "[rec-pull] ERROR: $label: the copy does not match the box (size $loc_size vs $box_size, sha256 ${loc_sha:0:12} vs ${box_sha:0:12}) -- removed" >&2
    return 1
  fi
  awk -v b="$box_size" -v a="$t0" -v z="$t1" -v cap="$rate" -v l="$label" -v s="${box_sha:0:12}" 'BEGIN {
    d = z - a; if (d <= 0) d = 0.001
    printf "[rec-pull] %s: %d B in %.1f s = %.1f Mbit/s (cap %d Mbit/s), sha256 %s match\n", l, b, d, b * 8 / d / 1e6, cap, s
  }'
  return 0
}
