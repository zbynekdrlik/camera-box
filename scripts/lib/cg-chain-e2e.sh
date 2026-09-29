#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions; the only top-level statement is the guarded
# win-ssh-exec.sh source below) — matches the sibling scripts/lib/*.sh convention (cold-cut-step.sh,
# camera-box-restart-verify.sh) of NOT setting `set -euo pipefail` in THIS file: sourcing runs in
# the CALLER's shell, and recording-e2e.sh (the only caller) already sets it and already sourced
# win-ssh-exec.sh, so the guarded source is a no-op there. A caller that did NOT source it first
# (a test) gets win-ssh-exec.sh's own top-level `set -euo pipefail` with it — the same flags every
# caller of this lib runs under anyway. Every function ALWAYS `return 0` on its
# best-effort (runtime) paths so a no-op / failed branch can never trip the caller's `set -e`
# (the sourced-`set -e`-leak class, .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/cg-chain-e2e.sh — #1301/#1302 opt-in CG_CHAIN=1 E2E profile for the
# SongPlayer-originated content chain (SongPlayer -> cg OBS (RESOLUME-SNV) -> strih -> stream).
# OFF BY DEFAULT (CG_CHAIN unset/0 ⇒ every function the harness calls is a pure no-op, so a normal
# E2E run is byte-for-byte inert). Invoked from recording-e2e.sh with the #675 sourced-lib pattern —
# the CG_CHAIN-guarded call lines are added AFTER existing anchored lines, never editing one.
#
# What the profile does when CG_CHAIN=1 (ALL best-effort + loud, never a camera-chain abort):
#   - at [5/8]: cut cg OBS program to the scene carrying the SongPlayer output (`sp-fast`, under the
#     Cut transition), put SongPlayer's OWN program on it through SongPlayer's obs-websocket facade
#     (:4456, its previous program snapshotted first), read BOTH programs back, then turn the
#     SongPlayer output burn ON through the SHIPPED API (POST {base}/api/v1/ndi/burn
#     {"output":"SP-fast","on":true}, every answer logged with its HTTP code) and POLL it back on
#     {base}/api/v1/ndi/health (`burn_on` of that output), turn the cg OBS's OWN hop burn (run
#     911015) ON on the input carrying that output (`sp-fast_video`, obs_burn_filter.py add + check —
#     only after a verified SongPlayer burn and both programs read back), and StartRecord cg OBS over
#     OBS-WS. No cg recording started ⇒ both burns go straight back OFF.
#   - after the camera sweep/hold, BEFORE [7/8] StopRecord: ONE tail CG window — strih program is
#     HARD-CUT (Cut transition, never a blend) to the scene carrying the `CG-obs` input (only that
#     item shown), so the strih + stream recordings carry the CG chain. It is a TAIL window on
#     purpose: a window inside switch-schedule.json would have no cam2 tick (a zero-frame cambox
#     window FAILS) and a mid-run cut would land inside the optical span. The tail placement + the
#     hard cut are DESIGNED to keep the camera-chain verdict out of it (the CG frames are a trailing
#     no-tick run after the last optical read); the first live CG_CHAIN=1 run is what confirms it.
#   - right after [7/8] StopRecord (after the genlock-audit AFTER snapshot): StopRecord cg OBS
#     (keeping the StopRecord host path for the on-box decode), both burns OFF, strih's scene +
#     program + transition, SongPlayer's program (through the facade) AND cg OBS's program +
#     transition restored — so nothing CG runs during the on-box decodes.
#   - at [8/8] (issue 1302): recording-verdict-on-resolume.sh decodes that exact cg file IN PLACE on
#     RESOLUME-SNV (`--extract-partial cg`), in the background next to the strih/stream extracts, and
#     pulls back only the small partial; the merge takes it as `--merge-partials cg=<json>` and emits
#     the REPORT-ONLY cg_chain section (src/cg_chain_gate.rs; never changes overall_pass). The
#     recording is never copied to dev1. A CG-LEG-VERIFIED / -SKIPPED / -NOT-VERIFIED run-log line
#     names the outcome; resolume away = SKIPPED, never a red.
#   - at [8/8a]/[8/8b] + the merge (issue 1302 slice 2): the strih/stream extracts and the merge get
#     `--cg-chain-burns` (the SongPlayer + cg ids join their expected-burn sets), and the merge gets
#     this run's `--cg-window`, so the strih/stream hops are judged inside the CG window at the
#     60->30 decimation step.
#   - in cleanup(): the #246/#844 leak-guard — right after the StopRecord-first block, ONE quick
#     background OFF for each burn this run owes an OFF (a cancelled job is SIGKILLed seconds later);
#     later the SongPlayer burn OFF (verified on /health) and the cg OBS hop burn OFF when this run
#     turned it on (verified by `check`), each retried with a SHORT per-request timeout and a loud
#     LEAK line if it never reads off, StopRecord cg OBS, every scene / program snapshot of this run
#     restored, and an in-flight background cg extract stopped, even on an early abort.
#   - not gated on CG_CHAIN (scripts/lib/cg-obs-burn-backstop.sh): rig-mode EVENT and the E2E
#     pre-run normalize also sweep the cg OBS burns while the traveling box is home — the backstop for
#     a cg hop burn a killed earlier run left on.

# win_ssh_scp_source_path (the backslash-to-slash scp source fix) lives in win-ssh-exec.sh, which
# recording-e2e.sh sources earlier; source it here too when a caller (a test) did not.
if ! declare -F win_ssh_scp_source_path >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/win-ssh-exec.sh
  . "$(dirname "${BASH_SOURCE[0]}")/win-ssh-exec.sh"
fi
# The SongPlayer API + SongPlayer's own program through its facade (cg-chain-songplayer.sh) and the
# home-gated cg OBS burn backstop (cg-obs-burn-backstop.sh, which rig-mode.sh sources on its own)
# live in their own files; both are source-only, no side effects at source time.
# shellcheck source=scripts/lib/cg-chain-songplayer.sh
. "$(dirname "${BASH_SOURCE[0]}")/cg-chain-songplayer.sh"
if ! declare -F cg_chain_backstop_sweep_targets >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/cg-obs-burn-backstop.sh
  . "$(dirname "${BASH_SOURCE[0]}")/cg-obs-burn-backstop.sh"
fi

# True iff the CG_CHAIN profile is enabled for this run. Pure, no side effects.
cg_chain_enabled() {
  [ "${CG_CHAIN:-0}" = "1" ]
}

# ---- hosts, scenes, state files ----------------------------------------------------------------

# Resolve the cg OBS (RESOLUME-SNV) host IP from DNS. PRINTS the IP on success; returns nonzero +
# a loud stderr line on failure (the caller treats a resolve failure as "skip the cg leg", never
# an abort). `$1` overrides the name (default resolume.lan).
cg_chain_resolve_host() {
  local name="${1:-${CG_CHAIN_HOST:-resolume.lan}}"
  local ip
  ip="$(getent hosts "$name" 2>/dev/null | awk '{print $1; exit}')"
  if [ -z "$ip" ]; then
    echo "[cg_chain] WARNING: could not resolve cg OBS host '$name' (getent hosts) — skipping the CG leg this run" >&2
    return 1
  fi
  printf '%s' "$ip"
}

# The cg OBS scene carrying the SongPlayer output (env CG_CHAIN_CG_SCENE, default the lower-cased
# output name — `SP-fast` -> `sp-fast`, the live cg_scenes naming). Pure.
cg_chain_cg_scene() {
  local out
  out="$(cg_chain_songplayer_output)"
  printf '%s' "${CG_CHAIN_CG_SCENE:-${out,,}}"
}

# The restore-snapshot path for kind $1 (cg-program | strih-scene), in CG_CHAIN_STATE_DIR (the
# run's OUTDIR; falls back to TMPDIR), keyed to RUN_ID when set — a reused OUTDIR never replays
# another run's snapshot. Pure.
cg_chain_state_file() {
  printf '%s/cg-chain-%s-state%s.json' "${CG_CHAIN_STATE_DIR:-${TMPDIR:-/tmp}}" "$1" "${RUN_ID:+-$RUN_ID}"
}

# The scene helper that sits next to obs_phase2.py ($1 = the obs_phase2.py path). Pure.
cg_chain_scene_py() {
  printf '%s/cg_chain_scene.py' "$(dirname "$1")"
}

# ---- the cg OBS hop burn (911015) ---------------------------------------------------------------
#
# The cg OBS paints its OWN hop burn (run 911015, BottomCenterRight) with the DistroAV burn filter on
# the input that carries the SongPlayer output. Like strih/stream it renders only while that input's
# `genlock_burn` is true — toggled over OBS-WS by obs_burn_filter.py `add` / `remove` and read back by
# its `check`. It is ON only for the cg recording: the cg OBS program feeds strih and, through Arena,
# possibly FOH/LED, and `genlock_burn` is saved in the cg OBS scene collection (it survives an OBS
# restart), so a burn left on is the #246/#844 leak class. CG_BURN_ON=1 means "this run owes an OFF":
# set just BEFORE the first `add` is sent (a signal can land between the add and its read-back), kept
# after an OFF that could not be verified, cleared only by a verified OFF. after_stoprecord and
# cleanup() turn the burn OFF only when it is set; no other tool sweeps the cg OBS.

# The cg OBS input carrying the SongPlayer output (env CG_CHAIN_CG_BURN_INPUT, default
# `<cg scene>_video` — the live `sp-fast_video`). Pure.
cg_chain_cg_burn_input() {
  printf '%s' "${CG_CHAIN_CG_BURN_INPUT:-$(cg_chain_cg_scene)_video}"
}

# The burn toggle that sits next to obs_phase2.py ($1 = the obs_phase2.py path). Pure.
cg_chain_burn_filter_py() {
  printf '%s/obs_burn_filter.py' "$(dirname "$1")"
}

# Classify an `obs_burn_filter.py check` answer ($1, any number of lines):
#   on       burn_on=True AND filter_enabled=True — the burn renders;
#   off      burn_on=False and genlock_burn is not True — nothing renders, and no filter re-enable
#            can bring it back;
#   unknown  anything else (no answer, a traceback, a disabled filter holding genlock_burn=True).
# Tokens are matched whole (space-delimited). Pure; ALWAYS returns 0.
cg_chain_cg_burn_check_state() {
  local ans=" ${1//$'\n'/ } "
  if [[ "$ans" == *" burn_on=True "* && "$ans" == *" filter_enabled=True "* ]]; then
    printf 'on'
  elif [[ "$ans" == *" burn_on=False "* && "$ans" != *" genlock_burn=True "* ]]; then
    printf 'off'
  else
    printf 'unknown'
  fi
  return 0
}

# The per-call budget (s) of one obs_burn_filter.py call at [5/8] and after [7/8] (env
# CG_CHAIN_BURN_OBS_TIMEOUT, default 10; a non-positive / non-integer value falls back to 10). Each
# call is a sub-second OBS-WS exchange; the record timeout (up to 90 s) would let a wedged cg OBS hold
# [5/8] for many minutes while strih and stream already record. cleanup() uses the shorter
# CG_CHAIN_CLEANUP_BURN_TIMEOUT instead. Pure.
cg_chain_burn_obs_timeout() {
  case "${CG_CHAIN_BURN_OBS_TIMEOUT:-}" in
    '' | *[!0-9]* | 0) printf '10' ;;
    *) printf '%s' "$CG_CHAIN_BURN_OBS_TIMEOUT" ;;
  esac
}

# Turn the cg OBS hop burn $1 (on|off) on cg_chain_cg_burn_input and VERIFY it with `check`.
# $2=cg-host-ip $3=path-to-obs_phase2.py $4=per-call timeout secs (every python call runs under it;
# default cg_chain_burn_obs_timeout). Up to CG_CHAIN_BURN_ATTEMPTS (default 3) toggle + read-back
# rounds, CG_CHAIN_BURN_RETRY_SLEEP seconds apart (default 1). CG_CHAIN_OBS_PASSWORD (the scene
# helper's) is passed as --password when set.
#   - ON is a no-op unless CG_CHAIN=1 (only the profile turns a burn on). It sets CG_BURN_ON=1 before
#     the first `add`. An ON that never verifies is a loud WARNING and is rolled straight back OFF:
#     the `add` may have reached OBS, and a half-known burn never stays on.
#   - A verified OFF sets CG_BURN_ON=0. An OFF that never verifies is a loud LEAK line naming the
#     manual off command, and CG_BURN_ON=1 so the next OFF (cleanup()) tries again.
# `check` proves the burn filter renders on that input; the run id (911015) comes from the cg OBS
# host role, and the verdict is what proves it. BEST-EFFORT; ALWAYS returns 0.
cg_chain_cg_burn() {
  local action="$1" host="${2:-}" py="${3:-}" tmo="${4:-}" verb input bf attempts i chk last
  local state=unknown pw=()
  case "$action" in
    on) verb=add ;;
    off) verb=remove ;;
    *)
      echo "[cg_chain] WARNING: cg OBS burn action '$action' is not on|off — nothing sent" >&2
      return 0
      ;;
  esac
  if [ "$action" = on ] && ! cg_chain_enabled; then return 0; fi
  case "$tmo" in '' | *[!0-9]* | 0) tmo="$(cg_chain_burn_obs_timeout)" ;; esac
  input="$(cg_chain_cg_burn_input)"
  bf="$(cg_chain_burn_filter_py "$py")"
  if [ -z "$host" ]; then
    if [ "$action" = off ]; then
      echo "[cg_chain] LEAK: no cg OBS host known — the cg OBS burn on '$input' was not turned off; turn it off: python3 $bf remove --host <cg OBS> --input '$input'" >&2
    else
      echo "[cg_chain] WARNING: no cg OBS host — the cg OBS burn was not turned on" >&2
    fi
    return 0
  fi
  if [ -n "${CG_CHAIN_OBS_PASSWORD:-}" ]; then pw=(--password "$CG_CHAIN_OBS_PASSWORD"); fi
  attempts="${CG_CHAIN_BURN_ATTEMPTS:-3}"
  case "$attempts" in '' | *[!0-9]* | 0) attempts=3 ;; esac
  if [ "$action" = on ]; then CG_BURN_ON=1; fi
  for ((i = 1; i <= attempts; i++)); do
    timeout "$tmo" python3 "$bf" "$verb" --host "$host" --input "$input" ${pw[@]+"${pw[@]}"} \
      >/dev/null 2>&1 || true
    chk="$(timeout "$tmo" python3 "$bf" check --host "$host" --input "$input" ${pw[@]+"${pw[@]}"} 2>&1 || true)"
    state="$(cg_chain_cg_burn_check_state "$chk")"
    if [ "$state" = "$action" ]; then
      if [ "$action" = off ]; then CG_BURN_ON=0; fi
      echo "[cg_chain] cg OBS burn $action VERIFIED by check (input '$input' at $host; the run id 911015 comes from the cg OBS host role)"
      return 0
    fi
    if [ "$i" -lt "$attempts" ]; then sleep "${CG_CHAIN_BURN_RETRY_SLEEP:-1}"; fi
  done
  last="${chk##*$'\n'}"
  if [ "$action" = on ]; then
    echo "[cg_chain] WARNING: cg OBS burn ON not confirmed after $attempts attempt(s) on '$input' ($host; last check: ${last:-no answer}) — rolling it back OFF; the cg hop is not measured this run" >&2
    cg_chain_cg_burn off "$host" "$py" "$tmo"
  else
    CG_BURN_ON=1
    echo "[cg_chain] LEAK: cg OBS burn still not OFF after $attempts attempt(s) on '$input' ($host; last check: ${last:-no answer}) — it may still be on the cg OBS output (strih, FOH/LED); turn it off: python3 $bf remove --host $host --input '$input'" >&2
  fi
  return 0
}

# ---- cg OBS record leg ---------------------------------------------------------------------------

# Cut cg OBS program to the scene carrying the SongPlayer output, snapshotting the previous program
# first (restored in cleanup()). $1=host-ip $2=path-to-obs_phase2.py $3=timeout-secs. BEST-EFFORT +
# loud: returns 0 on a successful cut, 1 on a failed one (a WARNING). MUST be called from an `if`.
cg_chain_cg_program_select() {
  local host="$1" py="$2" tmo="${3:-30}" scene
  scene="$(cg_chain_cg_scene)"
  if timeout "$tmo" python3 "$(cg_chain_scene_py "$py")" program --host "$host" --scene "$scene" \
    --state-file "$(cg_chain_state_file cg-program)" >/dev/null; then
    echo "[cg_chain] cg OBS ($host) program -> '$scene' OK"
    return 0
  fi
  echo "[cg_chain] WARNING: cg OBS ($host) program cut to '$scene' failed — the cg recording may not carry the SongPlayer burn" >&2
  return 1
}

# Cut cg OBS program to the SongPlayer scene (over :4455, the hard cut), put SongPlayer's own program
# on it through the facade, read BOTH programs back, and only then turn the SongPlayer burn ON, as
# the last SongPlayer step before the cg recording: SongPlayer registers a playlist's burn OFF
# whenever its pipeline (re)spawns (the flag is never persisted; it pre-creates one pipeline per
# active playlist at startup, a cut does not spawn one), and a burn with no playlist verified on
# program has nothing to mark. Then the cg OBS hop burn goes ON — only when the SongPlayer burn was
# VERIFIED on (CG_SP_BURN_ON=1) AND both programs read back on the scene, otherwise there is nothing
# for the cg hop to carry — and BEFORE StartRecord, so the cg recording carries it from its first
# frame; then StartRecord cg OBS over OBS-WS (obs_phase2.py record --host <ip> --action start). A
# failed StartRecord turns the cg burn straight back OFF (nothing will judge it); the caller turns
# the SongPlayer burn off. The burn and read-back calls run under cg_chain_burn_obs_timeout, never the
# record timeout. Args: $1=host-ip $2=path-to-obs_phase2.py $3=timeout-secs. Returns 0 on a started
# recording (caller then sets CG_RECORDING_STARTED=1 so cleanup() StopRecords this box), 1 on
# failure. MUST be called from an `if` (the nonzero return is the failure signal, not an abort —
# never call it bare under set -e).
cg_chain_record_start() {
  local host="$1" py="$2" tmo="${3:-30}" cut_ok=0
  if cg_chain_cg_program_select "$host" "$py" "$tmo" \
    && cg_chain_sp_program_select "$host" "$py" "$tmo" \
    && cg_chain_program_readback "$host" "$py"; then
    cut_ok=1
  fi
  if [ "$cut_ok" = 1 ]; then
    cg_chain_songplayer_burn on
  else
    echo "[cg_chain] WARNING: SongPlayer burn not turned on — the program cut did not read back on '$(cg_chain_cg_scene)'" >&2
  fi
  if [ "${CG_SP_BURN_ON:-0}" = 1 ] && [ "$cut_ok" = 1 ]; then
    cg_chain_cg_burn on "$host" "$py"
  else
    echo "[cg_chain] WARNING: cg OBS burn stays OFF (SongPlayer burn verified=${CG_SP_BURN_ON:-0}, cg program cut ok=$cut_ok) — the cg hop is not measured this run" >&2
  fi
  if timeout "$tmo" python3 "$py" record --host "$host" --action start >/dev/null 2>&1; then
    echo "[cg_chain] cg OBS ($host) StartRecord OK"
    return 0
  fi
  echo "[cg_chain] WARNING: cg OBS ($host) StartRecord failed — the cg_chain section will be omitted this run" >&2
  if [ "${CG_BURN_ON:-0}" = 1 ]; then cg_chain_cg_burn off "$host" "$py"; fi
  return 1
}

# StopRecord cg OBS over OBS-WS and KEEP the StopRecord host path (obs_phase2.py prints it as its
# only stdout line) in CG_HOST_RECORDING_PATH for the on-box decode. An empty answer (already
# stopped — the cleanup() pass) never clears a path an earlier stop recorded. Args: $1=host-ip
# $2=path-to-obs_phase2.py $3=timeout-secs. BEST-EFFORT + loud; ALWAYS return 0.
cg_chain_record_stop() {
  local host="$1" py="$2" tmo="${3:-30}" path
  if path="$(timeout "$tmo" python3 "$py" record --host "$host" --action stop 2>/dev/null)"; then
    path="$(printf '%s\n' "$path" | tail -n 1)"
    if [ -n "$path" ]; then CG_HOST_RECORDING_PATH="$path"; fi
    echo "[cg_chain] cg OBS ($host) StopRecord OK${path:+ -> $path}"
  else
    echo "[cg_chain] WARNING: cg OBS ($host) StopRecord failed (best-effort)" >&2
  fi
  return 0
}

# ---- (b) the cg OBS recording, decoded IN PLACE on RESOLUME-SNV (issue 1302) -----------------
#
# The cg recording is NEVER copied to dev1 (a 29-min dev1 decode blew the 75-min job budget on
# 25.9.2026): recording-verdict-on-resolume.sh decodes it ON the box that recorded it, the way the
# stream partial is extracted on the stream box, and pulls back only the small partial JSON (+ its
# pixel proofs). It is launched in the BACKGROUND right after the strih/stream extracts, so the wall
# time is max() of the legs, not their sum; the collect step waits for it at most a grace period past
# the camera legs, so a slow or wedged cg decode can never cost the job budget. Report-only end to
# end: every miss is a named CG-LEG marker, never a red.

# The ssh user / password for RESOLUME-SNV (env CG_CHAIN_USER / CG_CHAIN_PW). Pure.
cg_chain_user() {
  printf '%s' "${CG_CHAIN_USER:-newlevel}"
}
cg_chain_pw() {
  printf '%s' "${CG_CHAIN_PW:-newlevel}"
}

# This run's cg partial on dev1 (in CG_CHAIN_STATE_DIR, keyed to RUN_ID like the snapshots — a
# reused OUTDIR never merges another run's partial). Its `-pixels` sibling dir holds the pulled-back
# pixel proofs. Pure.
cg_chain_partial_file() {
  printf '%s/cg-partial%s.json' "${CG_CHAIN_STATE_DIR:-${TMPDIR:-/tmp}}" "${RUN_ID:+-$RUN_ID}"
}

# The box-local output dir (env CG_CHAIN_ONBOX_OUT_DIR, default the stream extract's
# `C:\camera-box\verdict-out`). The launch passes it as --out-dir AND builds the partial path from
# it, so the two can never point at different dirs. Pure.
cg_chain_onbox_out_dir_win() {
  printf '%s' "${CG_CHAIN_ONBOX_OUT_DIR:-C:\\camera-box\\verdict-out}"
}

# The same partial's path ON the box; its basename matches cg_chain_partial_file so the pull-back
# lands on exactly that dev1 path. Pure.
cg_chain_onbox_partial_win() {
  printf '%s\\cg-partial%s.json' "$(cg_chain_onbox_out_dir_win)" "${RUN_ID:+-$RUN_ID}"
}

# The background extract's log on dev1 (replayed into the run log by the collect step). Pure.
cg_chain_extract_log() {
  printf '%s/cg-extract%s.log' "${CG_CHAIN_STATE_DIR:-${TMPDIR:-/tmp}}" "${RUN_ID:+-$RUN_ID}"
}

# How long (s) the collect step still waits for the cg extract once it is reached — i.e. AFTER the
# strih/stream extracts are done (env CG_CHAIN_EXTRACT_GRACE_SECS, default 300; a non-integer value
# falls back to 300, 0 means "do not wait"). Why 300: the cg decode starts together with the camera
# extracts, which took 8-10 min on 25.9.2026, so it already had that long; 5 more minutes keeps the
# slowest run seen that day (the merge reached at minute ~47 of `timeout-minutes: 75`, with the
# merge + report + cleanup tail at ~1-2 min) far inside the job budget. A run that overruns reports
# CG-LEG-NOT-VERIFIED — look at the box, never raise the job timeout. Pure.
cg_chain_extract_grace_secs() {
  case "${CG_CHAIN_EXTRACT_GRACE_SECS:-}" in
    '' | *[!0-9]*) printf '300' ;;
    *) printf '%s' "$CG_CHAIN_EXTRACT_GRACE_SECS" ;;
  esac
}

# The ONE run-log marker for the cg leg, printed by the collect step. $1=dev1 partial path
# $2=state (`skipped` | `failed` | empty) $3=reason. Distinct greppable tokens:
#   CG-LEG-VERIFIED      the on-box cg partial reached dev1 and goes to the merge (which may still
#                        drop an unloadable one, with its own WARNING).
#   CG-LEG-SKIPPED       no cg leg this run BY DESIGN (resolume away / unresolvable, a plan-only run).
#   CG-LEG-NOT-VERIFIED  the cg leg was attempted and did not produce a partial.
# Pure (one `[ -f ]` + printf).
cg_chain_leg_marker() {
  local partial="${1:-}" state="${2:-}" reason="${3:-}"
  if [ "$state" != failed ] && [ "$state" != skipped ] && [ -n "$partial" ] && [ -f "$partial" ]; then
    printf 'CG-LEG-VERIFIED: the cg OBS partial decoded ON RESOLUME-SNV reached dev1 (%s) and goes to the merge for the report-only cg_chain section (issue 1302).\n' "$partial"
  elif [ "$state" = skipped ]; then
    printf 'CG-LEG-SKIPPED: %s — the report-only cg_chain section is omitted; the camera-chain gate is unaffected (issue 1302).\n' "$reason"
  else
    printf 'CG-LEG-NOT-VERIFIED: %s — the report-only cg_chain section is omitted; the camera-chain gate is unaffected (issue 1302).\n' "${reason:-no cg partial reached dev1}"
  fi
}

# Stop the decode this run started ON the box, bounded by CG_CHAIN_STOP_TIMEOUT (default 30 s):
# `recording-verdict-on-resolume.sh --stop-decode` stops only processes running from that one exe
# path, so the decode never keeps running next to the live Arena / cg OBS and never keeps the exe
# locked for the next run's upload. A no-op before a launch. ALWAYS returns 0.
cg_chain_onbox_decode_stop() {
  local here="${CG_EXTRACT_HERE:-}"
  [ -n "$here" ] && [ -n "${CG_HOST_IP:-}" ] || return 0
  RESOLUME_BOX="$CG_HOST_IP" RESOLUME_USER="$(cg_chain_user)" RESOLUME_PW="$(cg_chain_pw)" \
    timeout "${CG_CHAIN_STOP_TIMEOUT:-30}" "$here/recording-verdict-on-resolume.sh" --stop-decode >&2 \
    || echo "[cg_chain] WARNING: could not stop the on-box cg decode on $CG_HOST_IP (it may run to its end)" >&2
  return 0
}

# Stop an in-flight background cg extract: its whole dev1 process group (the script, sshpass and
# ssh), then the decode it started on the box (cg_chain_onbox_decode_stop). ALWAYS returns 0.
cg_chain_extract_stop() {
  local pid="${CG_EXTRACT_PID:-}"
  [ -n "$pid" ] || return 0
  kill -0 "$pid" 2>/dev/null || return 0
  kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
  echo "[cg_chain] stopped the in-flight cg OBS extract (pid $pid)" >&2
  cg_chain_onbox_decode_stop
  return 0
}

# Launch the cg OBS decode ON RESOLUME-SNV in the background. $1=scripts dir (recording-e2e.sh's
# $HERE) $2=dev1 path of the CI-built Windows recording-verdict.exe $3=E2E_EXECUTE_VERDICT. Sets
# CG_EXTRACT_PID on a launch; otherwise CG_LEG_STATE + CG_LEG_REASON name why nothing runs. MUST be
# called as a plain statement (never inside $(...)), so the background job belongs to the harness
# shell that later waits for it. A pure no-op unless CG_CHAIN=1; ALWAYS returns 0.
cg_chain_onbox_extract_launch() {
  cg_chain_enabled || return 0
  local here="$1" exe_local="${2:-}" execute="${3:-0}" partial log sess=setsid
  CG_EXTRACT_PID=""
  CG_EXTRACT_HERE="$here"
  CG_LEG_STATE=""
  CG_LEG_REASON=""
  partial="$(cg_chain_partial_file)"
  rm -rf -- "$partial" "${partial%.json}-pixels"
  if [ "${CG_RECORDING_STARTED:-0}" != 1 ]; then
    CG_LEG_STATE=skipped
    CG_LEG_REASON="no cg OBS recording this run (resolume away or unresolvable, or its StartRecord failed at [5/8])"
    return 0
  fi
  if [ -z "${CG_HOST_IP:-}" ] || [ -z "${CG_HOST_RECORDING_PATH:-}" ]; then
    CG_LEG_STATE=failed
    CG_LEG_REASON="cg OBS StopRecord returned no recording path"
    return 0
  fi
  if [ "$execute" != 1 ]; then
    CG_LEG_STATE=skipped
    CG_LEG_REASON="plan-only run (E2E_EXECUTE_VERDICT=0) — the cg decode runs only in the executing gate"
    return 0
  fi
  if [ -z "$exe_local" ] || [ ! -f "$exe_local" ]; then
    CG_LEG_STATE=failed
    CG_LEG_REASON="no Windows recording-verdict.exe on dev1 (WIN_VERDICT_EXE_LOCAL='$exe_local')"
    return 0
  fi
  log="$(cg_chain_extract_log)"
  # Its own process group, so the collect step (grace overrun) and cleanup() can stop the script
  # together with its sshpass/ssh children. Without job control (the harness) a background child is
  # not a group leader, so `setsid` execs in place and $! IS the new group's leader. With job control
  # on (`set -m`), bash already gives the job its own group — and setsid would fork, leaving $! a
  # parent that exits at once — so setsid is skipped there.
  case "$-" in *m*) sess="" ;; esac
  RESOLUME_BOX="$CG_HOST_IP" RESOLUME_USER="$(cg_chain_user)" RESOLUME_PW="$(cg_chain_pw)" \
    ${sess:+"$sess"} "$here/recording-verdict-on-resolume.sh" \
    --verdict-exe-local "$exe_local" --local-out-dir "$(dirname "$partial")" \
    --out-dir "$(cg_chain_onbox_out_dir_win)" \
    -- --extract-partial cg --cg "$CG_HOST_RECORDING_PATH" --out "$(cg_chain_onbox_partial_win)" \
    >"$log" 2>&1 &
  CG_EXTRACT_PID=$!
  CG_EXTRACT_STARTED="$(date +%s)"
  echo "    --- [8/8cg] #1302 cg OBS extract launched ON RESOLUME-SNV ($CG_HOST_IP) in the background (pid $CG_EXTRACT_PID, log $log) ---"
  return 0
}

# Collect the background cg extract: wait for it at most cg_chain_extract_grace_secs more (polling
# every CG_CHAIN_EXTRACT_POLL_SECS, default 5), stop it on an overrun, replay its log, log how long
# after its launch it was collected (the on-box STEP 2 time in the log is the calibration evidence)
# and print the CG-LEG marker. Any failure also asks the box to stop its decode — a dev1 side that
# died (an ssh drop) may have left it running. A failed
# / stopped extract leaves NO partial behind, so the merge never feeds a stale one. A pure no-op
# unless CG_CHAIN=1; ALWAYS returns 0.
cg_chain_onbox_extract_wait() {
  cg_chain_enabled || return 0
  local partial pid="${CG_EXTRACT_PID:-}" grace poll waited=0 rc=0 ran stopped=0
  partial="$(cg_chain_partial_file)"
  if [ -n "$pid" ]; then
    grace="$(cg_chain_extract_grace_secs)"
    case "${CG_CHAIN_EXTRACT_POLL_SECS:-}" in '' | *[!0-9]* | 0) poll=5 ;; *) poll="$CG_CHAIN_EXTRACT_POLL_SECS" ;; esac
    echo "    [8/8cg] #1302 waiting for the cg OBS extract (at most ${grace}s more)..."
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt "$grace" ]; do
      if declare -F interruptible_sleep >/dev/null 2>&1; then interruptible_sleep "$poll"; else sleep "$poll"; fi
      waited=$((waited + poll))
    done
    ran=$(($(date +%s) - ${CG_EXTRACT_STARTED:-$(date +%s)}))
    if kill -0 "$pid" 2>/dev/null; then
      cg_chain_extract_stop
      stopped=1
      CG_LEG_STATE=failed
      CG_LEG_REASON="the cg OBS decode on RESOLUME-SNV was still running after ${ran}s from its launch (${grace}s past the camera extracts) — stopped so it can never cost the job budget"
    fi
    wait "$pid" 2>/dev/null || rc=$?
    CG_EXTRACT_PID=""
    echo "    ----- cg extract log ($(cg_chain_extract_log)) -----"
    cat "$(cg_chain_extract_log)" 2>/dev/null || true
    echo "    ------------------------------------"
    echo "    [8/8cg] cg extract collected ${ran}s after its launch (the measured on-box decode time is the STEP 2 line in the log above, when STEP 2 finished)"
    if [ -z "${CG_LEG_STATE:-}" ] && { [ "$rc" != 0 ] || [ ! -f "$partial" ]; }; then
      CG_LEG_STATE=failed
      CG_LEG_REASON="the cg OBS extract on RESOLUME-SNV failed (rc=$rc, collected ${ran}s after its launch — see its log above)"
    fi
    if [ "${CG_LEG_STATE:-}" = failed ]; then
      rm -rf -- "$partial" "${partial%.json}-pixels"
      if [ "$stopped" = 0 ]; then cg_chain_onbox_decode_stop; fi
    fi
  fi
  echo "    $(cg_chain_leg_marker "$partial" "${CG_LEG_STATE:-}" "${CG_LEG_REASON:-}")"
  return 0
}

# ---- (c) the ONE tail CG window on strih --------------------------------------------------------

# True iff the tail CG window should run: the profile is on AND the cg OBS recording started (no
# cg recording = nothing to judge, so strih is never cut). Pure.
cg_chain_window_due() {
  cg_chain_enabled && [ "${CG_RECORDING_STARTED:-0}" = 1 ]
}

# The CG window length in seconds (env CG_CHAIN_WINDOW_SECS, default 30; a non-positive or
# non-integer value falls back to 30). Pure.
cg_chain_window_secs() {
  case "${CG_CHAIN_WINDOW_SECS:-}" in
    '' | *[!0-9]* | 0) printf '30' ;;
    *) printf '%s' "$CG_CHAIN_WINDOW_SECS" ;;
  esac
}

# The CG window record path in CG_CHAIN_STATE_DIR, keyed to RUN_ID when set (like the snapshots —
# a reused OUTDIR never shows another run's window). Pure.
cg_chain_window_file() {
  printf '%s/cg-window%s.json' "${CG_CHAIN_STATE_DIR:-${TMPDIR:-/tmp}}" "${RUN_ID:+-$RUN_ID}"
}

# The timeout for the strih CG cut ($1 = the caller's timeout): the cut enumerates every scene AND
# runs obs_phase2's polled non-black check (OBS_BLACKCHECK_TIMEOUT_S, default 20 s; a non-integer
# value reads as 20), so it gets that budget + 30 s, never less than the caller's timeout. Pure.
cg_chain_window_cut_timeout() {
  local tmo="${1:-30}" bc="${OBS_BLACKCHECK_TIMEOUT_S:-20}" need
  case "$bc" in '' | *[!0-9]*) bc=20 ;; esac
  case "$tmo" in '' | *[!0-9]*) tmo=30 ;; esac
  need=$((bc + 30))
  if [ "$tmo" -gt "$need" ]; then printf '%s' "$tmo"; else printf '%s' "$need"; fi
}

# The ONE CG window record: {"kind":"cg","scene":…,"input":…,"start_ns":…,"end_ns":…}. Returns 1
# (prints nothing) unless start_ns < end_ns are integers. Pure.
cg_chain_window_json() {
  python3 -c '
import json, sys
scene, inp, s, e = sys.argv[1:5]
try:
    s, e = int(s), int(e)
except ValueError:
    sys.exit(1)
if s >= e:
    sys.exit(1)
print(json.dumps({"kind": "cg", "scene": scene, "input": inp, "start_ns": s, "end_ns": e}, separators=(",", ":")))
' "$1" "$2" "$3" "$4"
}

# The tail CG window: cut strih program to the scene carrying CG_CHAIN_STRIH_INPUT (default
# `CG-obs`; CG_CHAIN_STRIH_SCENE picks one when several carry it) with only that item shown, hold
# it for cg_chain_window_secs, and write the window to cg_chain_window_file. The strih snapshot is
# restored by cg_chain_after_stoprecord (right after [7/8] StopRecord) and again by cleanup().
# $1=strih-host $2=path-to-obs_phase2.py $3=timeout-secs (raised by cg_chain_window_cut_timeout to
# cover the non-black check). A pure no-op unless cg_chain_window_due; BEST-EFFORT + loud; ALWAYS
# return 0.
cg_chain_window() {
  cg_chain_window_due || return 0
  local strih="$1" py="$2" tmo="${3:-30}" input out start_ns scene secs end_ns json
  input="${CG_CHAIN_STRIH_INPUT:-CG-obs}"
  secs="$(cg_chain_window_secs)"
  if ! out="$(timeout "$(cg_chain_window_cut_timeout "$tmo")" python3 "$(cg_chain_scene_py "$py")" strih-solo --host "$strih" \
    --input "$input" --scene "${CG_CHAIN_STRIH_SCENE:-}" --state-file "$(cg_chain_state_file strih-scene)")"; then
    echo "[cg_chain] WARNING: strih CG cut failed (input '$input') — strih/stream will not carry the CG chain this run" >&2
    return 0
  fi
  start_ns="${out%%$'\t'*}"
  scene="${out#*$'\t'}"
  echo "[6/8] #1302 CG window: strih program -> '$scene' ('$input' only) for ${secs}s, open (after the non-black check) at ${start_ns} ns"
  if declare -F interruptible_sleep >/dev/null 2>&1; then interruptible_sleep "$secs"; else sleep "$secs"; fi
  end_ns="$(date +%s%N)"
  # The window record is the run's evidence of WHEN strih carried the CG chain (the live acceptance
  # reads it next to the verdict's cg_chain section); it is logged too, so it survives in the CI log.
  if json="$(cg_chain_window_json "$scene" "$input" "$start_ns" "$end_ns")"; then
    printf '%s\n' "$json" >"$(cg_chain_window_file)" \
      && echo "    CG window $json -> $(cg_chain_window_file)"
  else
    echo "[cg_chain] WARNING: bad CG window bounds (start=$start_ns end=$end_ns) — window record not written" >&2
  fi
  return 0
}

# ---- (d) the verdict inputs for the strih/stream hops (issue 1302 slice 2) ----------------------

# The recording-verdict flag that adds the SongPlayer + cg OBS burn ids to the strih/stream
# expected-burn sets: `--cg-chain-burns` when the profile is on, else NOTHING. The harness splices it
# as ${CG_CHAIN_BURN_FLAG:+"$CG_CHAIN_BURN_FLAG"}, so a normal run's extract argv is byte-identical.
# Pure.
cg_chain_extract_burn_flag() {
  if cg_chain_enabled; then printf '%s' '--cg-chain-burns'; fi
  return 0
}

# Append the CG merge inputs to the caller's MERGE_ARGS array: `--cg-chain-burns` (the merge's
# expected-burn check must match the extracts), plus `--cg-window <file>` when THIS run's window
# record exists (cg_chain_window_file is keyed to RUN_ID, so another run's window is never fed), plus
# `--merge-partials cg=<json>` when THIS run's on-box cg partial reached dev1 (issue 1302 — the cg
# OBS origin hop; no partial = no cg_chain section, never a red). The verdict judges the
# strih/stream cg_chain hops only inside the window. A no-op unless CG_CHAIN=1. ALWAYS returns 0.
cg_chain_merge_args_append() {
  cg_chain_enabled || return 0
  local win partial
  MERGE_ARGS+=(--cg-chain-burns)
  win="$(cg_chain_window_file)"
  if [ -f "$win" ]; then MERGE_ARGS+=(--cg-window "$win"); fi
  partial="$(cg_chain_partial_file)"
  if [ -f "$partial" ]; then MERGE_ARGS+=(--merge-partials "cg=$partial"); fi
  return 0
}

# Restore one snapshot kind $1 through the scene helper ($2 = obs_phase2.py path, $3 = timeout).
# No snapshot file = nothing to do. ALWAYS return 0.
cg_chain_restore_snapshot() {
  local kind="$1" py="$2" tmo="${3:-30}" state
  state="$(cg_chain_state_file "$kind")"
  [ -f "$state" ] || return 0
  if timeout "$tmo" python3 "$(cg_chain_scene_py "$py")" restore --state-file "$state" >/dev/null; then
    echo "[cg_chain] $kind restored"
  else
    echo "[cg_chain] WARNING: $kind restore failed — snapshot kept at $state (restore by hand: python3 $(cg_chain_scene_py "$py") restore --state-file $state)" >&2
  fi
  return 0
}

# End the CG leg right after [7/8] StopRecord: StopRecord cg OBS (keeping the host path for the
# on-box decode), the SongPlayer burn OFF and the cg OBS hop burn OFF (each verified; the hop burn
# only when this run turned it on, CG_BURN_ON=1), strih's CG-window scene + program + transition
# restored, SongPlayer's program restored through its facade, then cg OBS's program + transition
# restored over :4455 (last, so cg OBS ends exactly as its snapshot recorded it) — so neither the cg
# recording nor a burn nor any CG program change runs through the long on-box decodes.
# cleanup() repeats every step (each is idempotent). $1=cg-host-ip-or-empty $2=path-to-obs_phase2.py
# $3=timeout-secs. A pure no-op unless CG_CHAIN=1; ALWAYS return 0.
cg_chain_after_stoprecord() {
  cg_chain_enabled || return 0
  local host="$1" py="$2" tmo="${3:-30}"
  if [ "${CG_RECORDING_STARTED:-0}" = 1 ] && [ -n "$host" ]; then
    cg_chain_record_stop "$host" "$py" "$tmo"
  fi
  cg_chain_songplayer_burn off
  if [ "${CG_BURN_ON:-0}" = 1 ]; then cg_chain_cg_burn off "$host" "$py"; fi
  cg_chain_restore_snapshot strih-scene "$py" "$tmo"
  cg_chain_sp_program_restore "$py" "$tmo"
  cg_chain_restore_snapshot cg-program "$py" "$tmo"
  return 0
}

# cleanup()'s FIRST pass, called right after the StopRecord-first block: a cancelled job gets a
# SIGINT and a SIGKILL a few seconds later, long before cleanup() reaches cg_chain_cleanup (after the
# camera restores), and a cg OBS burn left on survives in its scene collection. So this sends ONE
# quick OFF for each burn this run owes an OFF — the cg OBS hop burn when CG_BURN_ON=1, the SongPlayer
# burn when CG_SP_BURN_OWED=1 — each call under CG_CHAIN_CLEANUP_BURN_TIMEOUT (default 3 s). Each OFF
# is its OWN background job (CG_EARLY_BURNS_PIDS), the cg one launched first, so neither waits for the
# other and neither delays the camera device restores that follow. Their lines carry a "first pass"
# tag: a one-try LEAK here is not the verdict — cg_chain_cleanup waits for both jobs, then repeats
# both OFFs with retries (the authoritative report). Sends nothing when no burn is owed (every run
# whose [7/8] OFFs verified, every abort before [5/8]). $1=cg-host-ip-or-empty
# $2=path-to-obs_phase2.py. A pure no-op unless CG_CHAIN=1; ALWAYS returns 0.
cg_chain_cleanup_burns_first() {
  cg_chain_enabled || return 0
  local host="$1" py="$2" t="${CG_CHAIN_CLEANUP_BURN_TIMEOUT:-3}"
  local tag='    [cg_chain cleanup first pass: one try, cg_chain_cleanup retries] '
  CG_EARLY_BURNS_PIDS=""
  if [ "${CG_BURN_ON:-0}" = 1 ]; then
    { CG_CHAIN_BURN_ATTEMPTS=1 cg_chain_cg_burn off "$host" "$py" "$t"; } 2>&1 | sed -u "s/^/$tag/" &
    CG_EARLY_BURNS_PIDS="$!"
  fi
  if [ "${CG_SP_BURN_OWED:-0}" = 1 ]; then
    { CG_CHAIN_BURN_ATTEMPTS=1 CG_CHAIN_BURN_TIMEOUT="$t" cg_chain_songplayer_burn off; } 2>&1 \
      | sed -u "s/^/$tag/" &
    CG_EARLY_BURNS_PIDS="${CG_EARLY_BURNS_PIDS:+$CG_EARLY_BURNS_PIDS }$!"
  fi
  if [ -n "$CG_EARLY_BURNS_PIDS" ]; then
    echo "[cg_chain] cleanup first pass: burn OFFs sent in the background (pids $CG_EARLY_BURNS_PIDS)"
  fi
  return 0
}

# cleanup() leak-guard: the SongPlayer burn OFF and the cg OBS hop burn OFF (each verified; the hop
# burn only when CG_BURN_ON=1), StopRecord cg OBS, and every scene / program snapshot of this run
# restored (strih, SongPlayer through its facade, then cg OBS), even on an early abort. The burn requests use a SHORT per-request timeout
# (CG_CHAIN_CLEANUP_BURN_TIMEOUT, default 3 s), so an unreachable SongPlayer or cg OBS costs ~20 s
# each, never a minute in front of the stream/strih teardowns that follow. A pure no-op when CG_CHAIN
# is not enabled (so it is safe to call unconditionally from cleanup()). Args: $1=cg-host-ip-or-empty
# $2=path-to-obs_phase2.py $3=timeout-secs. ALWAYS return 0.
cg_chain_cleanup() {
  cg_chain_enabled || return 0
  local host="$1" py="$2" tmo="${3:-30}"
  # The first pass (cg_chain_cleanup_burns_first) is bounded by its own per-call timeouts; wait for
  # its jobs so their OFFs and the retries below never interleave.
  local p
  for p in ${CG_EARLY_BURNS_PIDS:-}; do
    wait "$p" 2>/dev/null || true
  done
  CG_EARLY_BURNS_PIDS=""
  # Issue 1302: an aborted run must not leave the background cg extract running.
  cg_chain_extract_stop
  CG_CHAIN_BURN_TIMEOUT="${CG_CHAIN_CLEANUP_BURN_TIMEOUT:-3}" cg_chain_songplayer_burn off
  if [ "${CG_BURN_ON:-0}" = 1 ]; then
    cg_chain_cg_burn off "$host" "$py" "${CG_CHAIN_CLEANUP_BURN_TIMEOUT:-3}"
  fi
  # Only a cg recording THIS run started (the #649 harness-started-boxes-only rule): CG_HOST_IP is
  # set as soon as the host resolves, before StartRecord.
  if [ "${CG_RECORDING_STARTED:-0}" = 1 ] && [ -n "$host" ]; then
    cg_chain_record_stop "$host" "$py" "$tmo"
  fi
  cg_chain_restore_snapshot strih-scene "$py" "$tmo"
  cg_chain_sp_program_restore "$py" "$tmo"
  cg_chain_restore_snapshot cg-program "$py" "$tmo"
  return 0
}
