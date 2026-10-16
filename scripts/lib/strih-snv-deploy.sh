#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions + constants and the per-box maps, nothing executed) --
# the scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing executes it in
# the CALLER's shell (deploy-genlock-fleet.sh sets its own strict mode). Every step below checks its
# own rc explicitly and returns a named failure, because the caller runs it in an `||` context where
# errexit is off by design.
#
# scripts/lib/strih-snv-deploy.sh -- issue 1317 part 6: the strih-snv EXECUTE arm of
# scripts/deploy-genlock-fleet.sh, plus the builders its --plan arm prints (the remote commands are
# the same strings in both).
#
# issue 1426: the arm is per BOX. Every obs-fleet linux-genlock box with a scripts/strih-boxes/<box>.env
# fact file (strih-snv, strih-pp, stream-pp; fleet_box_runs_setup_strih) runs it with its own name:
# `setup-strih.sh --box <box>`, `verify-strih.sh --box <box>`, errors as `[<box> <step>]`. A traveling
# box is dialled by its obs-fleet NAME (strih-pp.lan: dev1 reaches the PP LAN through companion-pp),
# never its fact file's STRIH_IP; one that does not answer over ssh is AWAY (strih_lx_apply rc 6, one
# line, nothing changed, never a failure). The rig-busy guard reads the box's venue (strih_lx_guard_hosts).
# strih_lx_apply_all rolls the boxes in order: an acceptance failure stops only that box (exit 5).
#
# strih-snv (10.77.9.202, a linux-genlock box since the M4 cut-over) installs the strih FULL genlock
# artifact through scripts/setup-strih.sh (release-parity gate, runtime packages, /usr prefix,
# chrome-sandbox) and runs OBS under the strih-obs.service user unit. Before this lib the supervisor
# deployed it with an ad-hoc scratch script that twice failed half-silently: the box's /tmp quota was
# full (each staged bundle is ~2.2 GB on a 7.5 GB tmpfs), rsync died `Disk quota exceeded` (rc 11),
# and the old build simply kept running with nothing refusing.
#
# Two phases, so deploy-genlock-fleet.sh can resolve EVERY requested box before changing any:
#
#   strih_lx_prepare (exit 3 on failure, the box is never touched)
#     resolve  -- the linux-genlock.yml run at the anchor's SAME SHA; the dial host must be a dotted
#                 IPv4 (the fleet row is one; a hostname / flag-shaped override is refused up front);
#     download -- the strih FULL artifact, REFUSED unless its GENLOCK_BUILD_SHA.txt is the canonical
#                 SHA and its BUNDLE_MANIFEST.json carries the libobs.so.30 sha256 the read-back uses;
#     tree     -- the COMMITTED scripts/ systemd/ intercom/ + vendor/realtek-r8152 (the rig NIC driver
#                 source setup-strih step 1b builds through DKMS, issue 1391) + the vendored dock headers
#                 the program-audio sampler step 16e builds its shim from (issue 1404; the list is
#                 strih_lx_tree_paths) of this checkout (an archive
#                 of HEAD) + the generated run-setup.sh, checked for the box's fact file scripts/strih-boxes/strih-snv.env
#                 and the intercom routing file it names; the GH token (STRIH_LX_GH_TOKEN, else
#                 `gh auth token`). setup-strih.sh runs as `setup-strih.sh --box strih-snv` (issue 1361).
#   strih_lx_apply (exit 4 on failure, every message names its step)
#     preflight -- refuse unless the box is the fact file's host, while a previous setup-strih.sh
#                  still runs on the box, and while a broadcast is LIVE: the ONE shared rig-busy guard
#                  stray_session_check_assert (scripts/lib/stray-session-check.sh, sourced by the
#                  caller; .claude/rules/rig-mutation-broadcast-guard.md) reads strih (the strih-snv
#                  dial IP) + stream (the obs-fleet row) immediately before the first mutation --
#                  streaming/recording on either = exit 4 naming what is live, nothing changed. Its
#                  fail-OPEN (WARN + proceed when NO box is readable) is the shared semantics, not
#                  redefined here -- no job-start gate precedes this deploy, so that is an ACCEPTED
#                  risk (both OBS WebSockets unreadable while the strih-snv ssh answers; a strih OBS
#                  that is down is often exactly what the deploy repairs);
#     sweep     -- the stage is created + touched NEWEST, then the EXISTING obs-backup-retention.sh
#                  --local-sweep decision (--stages-only, keep the newest 1, as the operator -- the
#                  stage dirs are the operator's) removes every older /tmp/genlock-stage-<sha>; a stage
#                  it could not remove is a loud WARNING (the rsync below is the hard gate);
#     stage     -- rsync bundle/ + repo/ into /tmp/genlock-stage-<sha>/ WHILE THE OLD OBS KEEPS
#                  RUNNING: a failed rsync exits 4 before anything is stopped;
#     stop      -- the SAME rig-busy guard again first (the stage can take minutes; a broadcast that
#                  went live meanwhile = exit 4 in step stop, OBS NOT stopped), then
#                  delegated to the sanctioned stop code (/usr/local/bin/strih-obs-stop.sh, which
#                  routes through `systemctl --user stop`) + a bounded wait; the deploy itself sends no
#                  kill (the stop code's own SIGTERM->grace->SIGKILL ladder is the unit's contract);
#     setup     -- setup-strih.sh as root, DETACHED on the box (an ssh drop cannot kill it mid-apt),
#                  the GH token on the launch's STDIN only (never an argv, never a file), rc polled;
#                  OBS is never started over an installer that is still running;
#     start     -- systemctl --user start strih-obs.service (after touching a start marker), then the
#                  issue-1399 session apps (strih-browser-keeper + bkshading-panel-app; the rc is OBS's);
#     verify    -- REFUSE unless /opt/obs-genlock/GENLOCK_BUILD_SHA.txt == the canonical SHA, the
#                  installed /usr/lib/x86_64-linux-gnu/libobs.so.30 bytes match the bundle manifest,
#                  strih-obs.service is active with the SAME MainPID and NRestarts for a settle time
#                  (a crash-looping Type=simple unit reads `active` between restarts), the OBS log
#                  written after the start shows `render tick ENABLED`, and :8899 reports the SHA.
#
# Every failure prints `ERROR: [<box> <step>] failed (rc=N): <what>` on stderr. Exit 5 = installed,
# running and read back, but the in-deploy verify-strih.sh acceptance gate is not clear.
#
# Test seams (tests/deploy_genlock_fleet_strih_lx_exec_1317.rs stubs gh/sshpass/ssh/rsync/curl on
# PATH): STRIH_LX_SETUP_POLLS / STRIH_LX_SETUP_POLL_SECS (setup rc poll, default 270 x 10 s = 45 min),
# STRIH_LX_VERIFY_POLLS / STRIH_LX_VERIFY_POLL_SECS (read-back poll, default 24 x 10 s = 4 min),
# STRIH_LX_VERIFY_SETTLE_SECS (how long the MainPID/NRestarts must hold, default 90 s),
# STRIH_LX_SSH_TIMEOUT (per remote command, default 180 s), STRIH_LX_OBS_PHASE2_DIR (the dir holding
# the obs_phase2.py the rig-busy guard runs, default this lib's scripts/ -- the test points it at a
# fake so no test opens a WebSocket to the rig; mirrors BKSHADING_DEPLOY_OBS_PHASE2_DIR).
# Transport env: STRIH_LX_IP (dial override only, via fleet_box_ip -- must be an IPv4), STRIH_LX_USER
# / STRIH_LX_PW (default newlevel / newlevel -- the rig's shared Linux-box creds, targets.md; sshpass
# -p is the repo-wide convention), STRIH_LX_GH_TOKEN (a read-only token instead of the operator's).

# issue 1399: the strih session apps' unit names (STRIH_SESSION_APP_UNITS), which the start step
# starts after OBS -- the ONE list setup-strih installs and verify-strih grades.
if ! declare -F strih_session_apps_autostart_lines >/dev/null; then
  # shellcheck source=scripts/lib/strih-session-apps.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-session-apps.sh"
fi
# issue 1404: setup-strih step 16e installs the program-audio sampler from STRIH_PROGRAM_AUDIO_FILES,
# which names two vendored dock headers outside scripts/ -- the staged tree must carry them.
# issue 1426: the ONE fact-file parser (strih_box_parse_file, never executes the file) and the ONE
# venue table (avsync_venue_host: a venue's stream box + how dev1 dials it), both read from this
# checkout's scripts/strih-boxes -- the same files the plan and the execute arm read.
if ! declare -F avsync_venue_host >/dev/null; then
  # shellcheck source=scripts/lib/avsync-venues.sh
  . "$(dirname "${BASH_SOURCE[0]}")/avsync-venues.sh"
fi
if ! declare -p STRIH_PROGRAM_AUDIO_FILES >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/strih-program-audio.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-program-audio.sh"
fi

# strih_lx_tree_paths -> the repo paths the provisioning tree archives, one per line: the dirs
# setup-strih.sh / verify-strih.sh read (scripts/ systemd/ intercom/ + the vendored rig NIC driver)
# plus every STRIH_PROGRAM_AUDIO_FILES entry outside them (issue 1404: the decoder shim's vendored
# av-sync-dock headers). Live 8.10.2026: a tree without them failed setup-strih step 16e.
strih_lx_tree_paths() {
  local base=(scripts systemd intercom vendor/realtek-r8152) f d covered
  printf '%s\n' "${base[@]}"
  for f in "${STRIH_PROGRAM_AUDIO_FILES[@]}"; do
    covered=0
    for d in "${base[@]}"; do
      case "$f" in "$d"/*) covered=1 ;; esac
    done
    [ "$covered" = 1 ] || printf '%s\n' "$f"
  done
}

STRIH_LX_STAGE_PARENT="/tmp"
STRIH_LX_SSH_OPTS="-o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no -o LogLevel=ERROR -o ConnectTimeout=12 -o ServerAliveInterval=15 -o ServerAliveCountMax=4"
STRIH_LX_RETENTION_ARGS="--local-sweep --stages-only --stage-parent /tmp --keep-runs 1 --keep-days 0 --execute"
# issue 1426: per-box values strih_lx_prepare resolves for every requested setup-strih box (the dial
# host, "<strih host> <stream host>" for the rig-busy guard, the fact-file hostname); strih_lx_apply
# loads one box's values into the STRIH_LX_PREP_* globals. STRIH_LX_BOX names the box being deployed.
declare -gA STRIH_LX_BOX_HOST=() STRIH_LX_BOX_GUARD=() STRIH_LX_BOX_HOSTNAME=()
STRIH_LX_BOX="strih-snv"

# strih_lx_stage_dir SHA -> /tmp/genlock-stage-<sha>, the obs-backup-retention allowlist shape
# (`^(stage-genlock|genlock-stage)-[0-9a-f]+$`) so a later sweep can prune it. A non-hex SHA is
# refused (rc 2) rather than turned into a name the sweep would never recognise. Pure.
strih_lx_stage_dir() {
  local sha="${1:-}"
  case "$sha" in
    ''|*[!0-9a-f]*) echo "strih_lx_stage_dir: SHA must be lowercase hex, got '$sha'" >&2; return 2 ;;
  esac
  printf '%s/genlock-stage-%s\n' "$STRIH_LX_STAGE_PARENT" "$sha"
}

# --- remote command builders (pure; the SAME text execute runs and --plan prints) ----------------

# the box's own hostname -- the identity check against the fact file's STRIH_HOSTNAME.
strih_lx_remote_identity_cmd() {
  printf '%s\n' 'hostname'
}

# setup-strih.sh step 17 runs verify-strih.sh; with no reboot pending it FAILS while the deploy has OBS
# stopped (item 1 "OBS running"). Prints how often that gate line is among the log's last 5 lines --
# 1 means every install step before it succeeded (fail() exits at the first failure).
strih_lx_remote_setup_gate_cmd() {
  printf "tail -n 5 '%s/setup-strih.log' 2>/dev/null | grep -F -c 'verify-strih.sh acceptance gate did not pass' || true\n" "$1"
}

# the real acceptance gate, run by the deploy once OBS is up (stdin = the sudo password line).
strih_lx_remote_accept_cmd() {
  printf "sudo -k -S -p '' '%s/repo/scripts/verify-strih.sh' --box %s\n" "$1" "$2"
}

strih_lx_remote_installer_cmd() {
  printf '%s\n' 'if pgrep -x setup-strih.sh >/dev/null; then echo alive; else echo idle; fi'
}

# create the stage dirs and touch the stage so it is the NEWEST stage dir -> the keep-newest-1 sweep
# can never delete the stage being deployed.
strih_lx_remote_prep_cmd() {
  printf "mkdir -p '%s/bundle' '%s/repo' && touch '%s'\n" "$1" "$1" "$1"
}

# the obs-backup-retention.sh --local-sweep leg (the leg `obs-backup-retention.sh --box strih-snv`
# runs), dialled at the deploy's own host, stage dirs only, as the operator: stdin = the retention
# script itself (from the committed tree) -- no sudo, so no password line ever reaches a shell.
strih_lx_remote_sweep_cmd() {
  printf 'bash -s -- %s\n' "$STRIH_LX_RETENTION_ARGS"
}

# the deploying stage must still be there; any OTHER stage dir the sweep left is printed LEFTOVER.
strih_lx_remote_stage_check_cmd() {
  printf "test -d '%s/bundle' && test -d '%s/repo' || exit 3; for d in /tmp/genlock-stage-* /tmp/stage-genlock-*; do if [ -d \"\$d\" ] && [ \"\$d\" != '%s' ]; then echo \"LEFTOVER \$d\"; fi; done; exit 0\n" "$1" "$1" "$1"
}

# the graceful stop, delegated to strih-obs-stop.sh (plain mode routes through `systemctl --user
# stop`, so the unit's ExecStop runs and Restart=on-failure does not relaunch it), then a bounded wait
# for BOTH the unit and every obs process to be gone. Not gone in 30 s = exit 5 (a refusal); this
# command itself never sends a signal.
strih_lx_remote_stop_cmd() {
  # shellcheck disable=SC2016  # expanded on the box, not here
  printf '%s\n' 'export XDG_RUNTIME_DIR=/run/user/$(id -u); if [ -x /usr/local/bin/strih-obs-stop.sh ]; then /usr/local/bin/strih-obs-stop.sh || exit 5; else systemctl --user stop strih-obs.service || exit 5; fi; i=0; while systemctl --user is-active --quiet strih-obs.service || pgrep -x obs >/dev/null; do i=$((i+1)); if [ "$i" -gt 30 ]; then echo "strih-obs/obs still running 30 s after the graceful stop" >&2; exit 5; fi; sleep 1; done; echo "strih-obs stopped (graceful)"'
}

# launch the staged run-setup.sh as root. stdin = the sudo password line, then `ghtoken:<token>`
# (the runner skips a password line a NOPASSWD sudo left unread). `-k` ignores a cached credential.
strih_lx_remote_setup_launch_cmd() {
  printf "sudo -k -S -p '' bash '%s/repo/run-setup.sh'\n" "$1"
}

strih_lx_remote_setup_rc_cmd() {
  printf "cat '%s/setup-strih.rc' 2>/dev/null || true\n" "$1"
}

strih_lx_remote_setup_log_cmd() {
  printf "tail -n 60 '%s/setup-strih.log' 2>/dev/null || true\n" "$1"
}

# setup-strih.sh exits 0 with a reboot-pending warning (the baseline lands at the next boot): pass
# THAT line on instead of letting VERIFIED swallow it. Keyed on the warning's own box-agnostic text
# (`reboot <box>, then run verify-strih.sh --box <box>`, setup-strih.sh step 17; a test pins the
# coupling) -- the baseline prints many routine "next boot" lines earlier in the run.
strih_lx_remote_setup_notes_cmd() {
  printf "grep -F -e ', then run verify-strih' '%s/setup-strih.log' 2>/dev/null || true\n" "$1"
}

# touch the start marker (the read-back only trusts an OBS log written after it), then start OBS,
# then the issue-1399 session apps (the browser-source keeper, the shading panel window, Companion
# Satellite and its watch timer, and the issue-1429 dantesync tray), the same order the kiosk autostart
# starts them. The command's exit
# code is OBS's: a session app that does not start is a WARNING here, and verify-strih items 37-40 (the
# deploy's acceptance step) name it.
strih_lx_remote_start_cmd() {
  # shellcheck disable=SC2016  # expanded on the box, not here
  printf 'export XDG_RUNTIME_DIR=/run/user/$(id -u); touch %s; systemctl --user reset-failed strih-obs.service 2>/dev/null; systemctl --user start strih-obs.service; obs_rc=$?; systemctl --user start %s || echo "WARNING: [%s start] the session apps (%s) did not start -- verify-strih items 37-40 name why" >&2; exit "$obs_rc"\n' \
    "'$1/obs-start.marker'" "${STRIH_SESSION_APP_UNITS[*]}" "${2:-strih-snv}" "${STRIH_SESSION_APP_UNITS[*]}"
}

# one line `installed=<marker sha> active=<unit state> pid=<MainPID> restarts=<NRestarts>
# lib=<sha256 of the installed libobs.so.30> tick=<1 when the newest OBS log is newer than the start
# marker AND shows `render tick ENABLED`, else 0>`.
strih_lx_remote_readback_cmd() {
  # shellcheck disable=SC2016  # expanded on the box, not here
  printf 'export XDG_RUNTIME_DIR=/run/user/$(id -u); L=$(ls -t "$HOME"/.config/obs-studio/logs/*.txt 2>/dev/null | head -n 1); t=0; M=%s; if [ -n "$L" ] && [ -f "$M" ] && [ "$L" -nt "$M" ] && LC_ALL=C grep -a -q "render tick ENABLED" "$L"; then t=1; fi; printf "installed=%%s active=%%s pid=%%s restarts=%%s lib=%%s tick=%%s\\n" "$(tr -d "[:space:]" 2>/dev/null < /opt/obs-genlock/GENLOCK_BUILD_SHA.txt)" "$(systemctl --user is-active strih-obs.service 2>/dev/null)" "$(systemctl --user show -p MainPID --value strih-obs.service 2>/dev/null)" "$(systemctl --user show -p NRestarts --value strih-obs.service 2>/dev/null)" "$(sha256sum /usr/lib/x86_64-linux-gnu/libobs.so.30 2>/dev/null | cut -d" " -f1)" "$t"\n' "'$1/obs-start.marker'"
}

strih_lx_bundle_state_url() {
  printf 'http://%s:8899/bundle-state.json\n' "$1"
}

# strih_lx_setup_runner STAGE BOX -> the on-box run-setup.sh (staged at STAGE/repo/run-setup.sh).
# Run as root with stdin = the rest of the launch pipe: it reads the `ghtoken:` line (skipping a
# sudo password line a NOPASSWD sudo left unread) and re-execs itself DETACHED to run
# `setup-strih.sh --box BOX` (the box's fact file scripts/strih-boxes/BOX.env, issue 1361) and write
# its rc to STAGE/setup-strih.rc -- so an ssh drop mid-apt never kills the install, and the rc is
# explicit. Issue 1411: the detach is a SYSTEM transient unit (systemd-run), never setsid -- a setsid
# child stays in the ssh session scope under user-1000.slice, and step 11's desktop purge on a fresh
# box ends the last GUI session, so systemd stopped the user slice and killed setup-strih mid-apt
# (new strih notebook, 11.10.2026). A system unit inherits no environment: the token, the bundle
# dir and the CAM_PW / STRIH_NDI_PEER pass-through go into a 0600 env file the child sources and
# deletes at once, never into systemd-run's argv (ps, the unit's properties).
strih_lx_setup_runner() {
  local stage="$1" box="$2"
  printf '#!/bin/bash\n# run-setup.sh -- generated by scripts/lib/strih-snv-deploy.sh (issue 1317 part 6).\n'
  printf 'set -uo pipefail\n'
  printf 'S=%q\nBOX=%q\n' "$stage" "$box"
  cat <<'RUNNER'
if [ "${1:-}" = "--child" ]; then
  if [ -f "$S/.runner.env" ]; then . "$S/.runner.env"; fi
  rm -f "$S/.runner.env"
  "$S/repo/scripts/setup-strih.sh" --box "$BOX" > "$S/setup-strih.log" 2>&1
  echo "$?" > "$S/setup-strih.rc"
  exit 0
fi
[ "$(id -u)" = 0 ] || { echo "run-setup: must run as root" >&2; exit 2; }
GH_TOKEN=""
while IFS= read -r line; do
  case "$line" in ghtoken:*) GH_TOKEN="${line#ghtoken:}"; break ;; esac
done
[ -n "$GH_TOKEN" ] || { echo "run-setup: no ghtoken: line on stdin" >&2; exit 2; }
[ -x "$S/repo/scripts/setup-strih.sh" ] || { echo "run-setup: $S/repo/scripts/setup-strih.sh missing or not executable" >&2; exit 2; }
rm -f "$S/setup-strih.rc" "$S/setup-strih.log"
(
  umask 077
  printf 'export GH_TOKEN=%q STRIH_LX_BUNDLE_SRC=%q\n' "$GH_TOKEN" "$S/bundle"
  for v in CAM_PW STRIH_NDI_PEER; do
    if [ -n "${!v:-}" ]; then printf 'export %s=%q\n' "$v" "${!v}"; fi
  done
) > "$S/.runner.env" || { echo "run-setup: cannot write $S/.runner.env" >&2; exit 2; }
UNIT="$BOX-setup-$(date +%s)"
if ! systemd-run --unit="$UNIT" --collect --quiet bash "$0" --child; then
  rm -f "$S/.runner.env"
  echo "run-setup: systemd-run could not start the setup unit" >&2
  exit 2
fi
echo "run-setup: setup-strih.sh launched as system unit $UNIT (log $S/setup-strih.log, rc $S/setup-strih.rc)"
RUNNER
}

# --- pure verdicts --------------------------------------------------------------------------------

# strih_lx_deploy_verdict WANT INSTALLED ACTIVE BS_SHA WANT_LIB LIB TICK -> `OK` (rc 0) or
# `FAIL: <why>` lines (rc 1). Fail-closed: an empty value anywhere is a FAIL, never "unknown = fine".
strih_lx_deploy_verdict() {
  local want="${1:-}" installed="${2:-}" active="${3:-}" bs="${4:-}" want_lib="${5:-}" lib="${6:-}" tick="${7:-}" bad=0
  if [ -z "$want" ]; then echo "FAIL: no canonical SHA to compare against"; return 1; fi
  if [ "$installed" != "$want" ]; then
    echo "FAIL: /opt/obs-genlock/GENLOCK_BUILD_SHA.txt=${installed:-<unreadable>} != canonical ${want}"; bad=1
  fi
  if [ -z "$want_lib" ] || [ "$lib" != "$want_lib" ]; then
    echo "FAIL: installed /usr/lib/x86_64-linux-gnu/libobs.so.30 sha256=${lib:-<unreadable>} != the bundle manifest's ${want_lib:-<none>}"; bad=1
  fi
  if [ "$active" != "active" ]; then
    echo "FAIL: strih-obs.service is ${active:-<unreadable>}, not active"; bad=1
  fi
  if [ "$tick" != "1" ]; then
    echo "FAIL: no 'render tick ENABLED' in an OBS log written after the start"; bad=1
  fi
  if [ "$bs" != "$want" ]; then
    echo "FAIL: :8899 genlock_build_sha=${bs:-<unreadable>} != canonical ${want}"; bad=1
  fi
  [ "$bad" = 0 ] && { echo "OK"; return 0; }
  return 1
}

# strih_lx_stable_verdict PID1 RESTARTS1 PID2 RESTARTS2 -> `OK` (rc 0) when the SAME non-zero MainPID
# and an unchanged NRestarts were read on two consecutive polls; else `FAIL: <why>` (rc 1). Pure.
strih_lx_stable_verdict() {
  local p1="${1:-}" r1="${2:-}" p2="${3:-}" r2="${4:-}"
  case "$p1" in ''|0) echo "FAIL: strih-obs.service has no MainPID (${p1:-<unreadable>})"; return 1 ;; esac
  if [ "$p1" != "$p2" ] || [ "$r1" != "$r2" ]; then
    echo "FAIL: strih-obs.service restarted between polls (MainPID ${p1} -> ${p2:-<none>}, NRestarts ${r1:-?} -> ${r2:-?}) -- a crash-looping OBS"
    return 1
  fi
  echo "OK"
}

# strih_lx_is_ipv4 ADDR -> rc 0 for four dot-separated decimal octets 0..255. Pure.
strih_lx_is_ipv4() {
  local a="${1:-}" o
  [[ "$a" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]] || return 1
  local IFS=.
  for o in $a; do [ "$((10#$o))" -le 255 ] || return 1; done
  return 0
}

# strih_lx_is_dns_name NAME -> rc 0 for a plain lower-case DNS name of one or more dot-separated
# labels (letters, digits, inner hyphens; never a leading `-`, so never flag-shaped). The ASCII sets are
# spelled out: a bracket RANGE follows the locale (.claude/rules/ci-testing-gotchas.md). Pure.
strih_lx_is_dns_name() {
  local LC_ALL=C lower='abcdefghijklmnopqrstuvwxyz' digit='0123456789'
  local label="[${lower}${digit}]([${lower}${digit}-]*[${lower}${digit}])?"
  [[ "${1:-}" =~ ^${label}(\.${label})*$ ]]
}

# strih_lx_dial_host_check BOX HOST -> rc 0 when HOST is how the deploy may dial BOX (issue 1426): a
# `traveling` obs-fleet row (strih-pp, stream-pp) dials its fleet NAME, a plain DNS name that is not an
# address (dev1 reaches the PP LAN through companion-pp; the fact file's STRIH_IP is the venue address,
# never dialled, so a row that holds an IP is refused here); every other row (strih-snv, `always`) a
# dotted IPv4 (the fleet row is one). Else the named exit-3 refusal.
strih_lx_dial_host_check() {
  local box="$1" host="$2"
  if [ "$(obs_fleet_home_check "$box" 2>/dev/null)" = traveling ]; then
    if strih_lx_is_dns_name "$host" && ! [[ "$host" =~ ^[0123456789.]+$ ]]; then return 0; fi
    _strih_lx_fail resolve 2 3 "${box} host '$host' must be its obs-fleet NAME (a plain DNS name like ${box}.lan -- a traveling box is dialled by name, never by an address such as the venue STRIH_IP)"; return
  fi
  strih_lx_is_ipv4 "$host" && return 0
  _strih_lx_fail resolve 2 3 "${box} host '$host' must be a dotted IPv4 (the fleet row is one; a DNS name is refused here, and a flag-shaped or mistyped override is refused up front)"
}

# strih_lx_fact BOX KEY [DIR] -> the value of KEY in the box fact file DIR/BOX.env (default: this
# checkout's scripts/strih-boxes), through the ONE parser strih_box_parse_file (never sourced). Empty
# (rc 0) when the file or the key is absent; empty + rc 2 when the file does not parse (its own stderr
# lines name why). Pure.
strih_lx_fact() {
  local dir="${3:-$(dirname "${BASH_SOURCE[0]}")/../strih-boxes}" parsed line
  [ -f "$dir/$1.env" ] || return 0
  parsed="$(strih_box_parse_file "$dir/$1.env")" || return 2
  while IFS= read -r line; do
    case "$line" in "$2="*) printf '%s\n' "${line#*=}"; return 0 ;; esac
  done <<< "$parsed"
  return 0
}

# strih_lx_venue_strih_box VENUE [DIR] -> the ONE fact-file box of VENUE whose STRIH_ROLE is strih
# (strih-snv for snv, strih-pp for pp); rc 2 (no output) for none or more than one. Pure.
strih_lx_venue_strih_box() {
  local venue="$1" dir="${2:-$(dirname "${BASH_SOURCE[0]}")/../strih-boxes}" f b found=""
  for f in "$dir"/*.env; do
    [ -f "$f" ] || continue
    b="$(basename "$f" .env)"
    [ "$(strih_lx_fact "$b" STRIH_VENUE "$dir")" = "$venue" ] || continue
    [ "$(strih_lx_fact "$b" STRIH_ROLE "$dir")" = strih ] || continue
    [ -z "$found" ] || return 2
    found="$b"
  done
  [ -n "$found" ] || return 2
  printf '%s\n' "$found"
}

# strih_lx_guard_hosts BOX DIAL_HOST -> "<strih host> <stream host>": the venue the rig-busy guard reads
# before BOX's OBS is stopped (issue 1426). The venue is BOX's fact-file STRIH_VENUE. Its strih is BOX
# itself when BOX's STRIH_ROLE is strih (DIAL_HOST, so the strih-snv STRIH_LX_IP override is read where
# it is dialled), else the venue's one strih fact file (strih_lx_venue_strih_box) at its fleet host. Its
# stream is the ONE venue table's stream host (avsync_venue_host: a stream-role fact file, else the
# declared Windows stream-snv). strih-snv -> "<its dial IP> 10.77.9.204" exactly as before; stream-pp ->
# "strih-pp.lan stream-pp.lan". rc 2 + one stderr line naming the venue and the missing piece. Pure.
strih_lx_guard_hosts() {
  local box="$1" dial="$2" venue s t sbox
  venue="$(strih_lx_fact "$box" STRIH_VENUE)"
  [ -n "$venue" ] || { echo "strih_lx_guard_hosts: ${box} has no STRIH_VENUE fact" >&2; return 2; }
  if [ "$(strih_lx_fact "$box" STRIH_ROLE)" = strih ]; then
    s="$dial"
  else
    sbox="$(strih_lx_venue_strih_box "$venue")" && s="$(fleet_box_ip "$sbox")" \
      || { echo "strih_lx_guard_hosts: venue ${venue} has no single strih fact file with an obs-fleet row" >&2; return 2; }
  fi
  t="$(avsync_venue_host "$venue")" && [ -n "$t" ] \
    || { echo "strih_lx_guard_hosts: venue ${venue} has no stream box in the venue table (a stream-role fact file or a declared stream row)" >&2; return 2; }
  [ -n "$s" ] || return 2
  printf '%s %s\n' "$s" "$t"
}

# strih_lx_tree_check TREE BOX -> prints the fact file's STRIH_HOSTNAME (rc 0) when the staged tree
# holds what the box deploy needs: setup-strih.sh, verify-strih.sh, obs-backup-retention.sh,
# systemd/strih-obs.service, the box fact file scripts/strih-boxes/BOX.env (issue 1361) and the
# intercom routing file its STRIH_INTERCOM_CONFIG names (setup-strih.sh step 13 installs it). The fact
# file is read with sed, never sourced on dev1. rc 1 + a named reason on stderr otherwise. Pure.
strih_lx_tree_check() {
  local tree="$1" box="$2" env ic hn f
  env="$tree/scripts/strih-boxes/$box.env"
  for f in scripts/setup-strih.sh scripts/verify-strih.sh scripts/obs-backup-retention.sh systemd/strih-obs.service; do
    [ -f "$tree/$f" ] || { echo "strih_lx_tree_check: no $f" >&2; return 1; }
  done
  [ -f "$env" ] || { echo "strih_lx_tree_check: no scripts/strih-boxes/$box.env (the box fact file)" >&2; return 1; }
  ic="$(strih_lx_fact "$box" STRIH_INTERCOM_CONFIG "$tree/scripts/strih-boxes")" || ic=""
  [ -n "$ic" ] || { echo "strih_lx_tree_check: scripts/strih-boxes/$box.env has no STRIH_INTERCOM_CONFIG" >&2; return 1; }
  [ -f "$tree/$ic" ] || { echo "strih_lx_tree_check: no $ic (STRIH_INTERCOM_CONFIG) -- the intercom/ dir must be staged" >&2; return 1; }
  hn="$(strih_lx_fact "$box" STRIH_HOSTNAME "$tree/scripts/strih-boxes")" || hn=""
  [ -n "$hn" ] || { echo "strih_lx_tree_check: scripts/strih-boxes/$box.env has no STRIH_HOSTNAME" >&2; return 1; }
  for f in "${STRIH_PROGRAM_AUDIO_FILES[@]}"; do
    [ -f "$tree/$f" ] || { echo "strih_lx_tree_check: no $f (setup-strih step 16e installs the program-audio sampler from it, issue 1404)" >&2; return 1; }
  done
  printf '%s\n' "$hn"
}

# --- execute arm ----------------------------------------------------------------------------------

_strih_lx_fail() {  # STEP RC EXIT MESSAGE -> prints the named error (the box being deployed), returns EXIT
  echo "ERROR: [${STRIH_LX_BOX:-strih-snv} $1] failed (rc=$2): $4" >&2
  return "$3"
}

# _strih_lx_ssh CMD -> run CMD on the box, bounded by STRIH_LX_SSH_TIMEOUT (stdin passes through).
# `timeout` sits INSIDE sshpass so the password prompt still reaches sshpass's pty.
_strih_lx_ssh() {
  local -a opts
  read -r -a opts <<< "$STRIH_LX_SSH_OPTS"
  sshpass -p "$STRIH_LX_PREP_PW" timeout "${STRIH_LX_SSH_TIMEOUT:-180}" ssh "${opts[@]}" \
    "${STRIH_LX_PREP_USER}@${STRIH_LX_PREP_HOST}" "$1"
}

_strih_lx_rsync() {  # SRC_DIR/ DEST_DIR/ -- --timeout=120 is rsync's own I/O-stall bound
  sshpass -p "$STRIH_LX_PREP_PW" rsync -a --delete --timeout=120 -e "ssh $STRIH_LX_SSH_OPTS" \
    "$1" "${STRIH_LX_PREP_USER}@${STRIH_LX_PREP_HOST}:$2"
}

_strih_lx_field() {  # KEY LINE -> the value of KEY=<value> in a read-back line
  printf ' %s\n' "$2" | sed -n "s/.*[[:space:]]$1=\([^ ]*\).*/\1/p" | head -n 1
}

# strih_lx_obs_phase2_dir -> the dir holding the obs_phase2.py the rig-busy guard runs:
# STRIH_LX_OBS_PHASE2_DIR, else this lib's scripts/ dir.
strih_lx_obs_phase2_dir() {
  if [ -n "${STRIH_LX_OBS_PHASE2_DIR:-}" ]; then printf '%s\n' "$STRIH_LX_OBS_PHASE2_DIR"; return 0; fi
  (cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
}

# _strih_lx_broadcast_guard STEP WHAT_KEPT -> the ONE shared rig-busy guard (stray_session_check_assert)
# at the box's venue: its strih (the strih-snv dial IP for strih-snv) + its stream host
# (strih_lx_guard_hosts, issue 1426). It `exit 1`s on a refusal (written for bare-statement
# callers), so it runs in a subshell here: its stdout passes through, its stderr is re-printed and
# names what is live (stray_session_busy_summary). Returns 0, or 4 with `[<box> STEP]` naming what
# is live + WHAT_KEPT (what the box still has).
_strih_lx_broadcast_guard() {
  local step="$1" kept="$2" guard_err grc
  { guard_err="$( ( stray_session_check_assert "$STRIH_LX_PREP_OBS_PHASE2_DIR" "$STRIH_LX_PREP_GUARD_STRIH" "$STRIH_LX_PREP_STREAM_HOST" "the ${STRIH_LX_BOX} OBS deploy (stop + setup-strih.sh)" ) 2>&1 1>&3 3>&- )"; grc=$?; } 3>&1
  [ -n "$guard_err" ] && printf '%s\n' "$guard_err" >&2
  [ "$grc" = 0 ] && return 0
  _strih_lx_fail "$step" "$grc" 4 "a broadcast is LIVE on strih/stream: $(stray_session_busy_summary "$guard_err") -- refusing to stop the ${STRIH_LX_BOX} OBS; ${kept} (ONLY if the hint above names our own leftover recording -- recording ON, streaming OFF -- stop it over OBS-WS and re-run; never a live production)"
}

_strih_lx_start_best_effort() {
  echo "# ${STRIH_LX_BOX}: starting the installed OBS best-effort so the box is not left dark" >&2
  _strih_lx_ssh "$(strih_lx_remote_start_cmd "$STRIH_LX_PREP_STAGE" "$STRIH_LX_BOX")" || true
}

_strih_lx_installer_state() {  # -> alive | idle | unreachable (the ssh itself failed)
  local out
  out="$(_strih_lx_ssh "$(strih_lx_remote_installer_cmd)" 2>/dev/null)" || { echo unreachable; return 0; }
  case "$(printf '%s' "$out" | tr -d '[:space:]')" in
    idle) echo idle ;;
    alive) echo alive ;;
    *) echo unreachable ;;
  esac
}

# strih_lx_prepare SHA WORKDIR REPO GH_REPO [BOXES] -> everything that needs no box: resolve, download,
# the provisioning tree, the token. BOXES (default strih-snv; issue 1426: any comma list of setup-strih
# boxes) share ONE download + tree; each box's dial host, guard hosts, fact-file hostname and runner are
# resolved here too (the STRIH_LX_BOX_* maps), so every box is resolved before any box is touched. Sets
# the STRIH_LX_PREP_* globals strih_lx_apply reads. 0 or 3.
strih_lx_prepare() {
  local sha="$1" work="$2" repo="$3" gh_repo="$4" boxes="${5:-strih-snv}"
  local w="$work/strih-snv" art lrun rc art_sha tree_rev tool f b host guard
  local -a box_list
  IFS=',' read -r -a box_list <<< "$boxes"
  STRIH_LX_BOX="$boxes"   # a shared step's error names every box it serves
  STRIH_LX_PREP_WORK="$w"
  STRIH_LX_PREP_USER="${STRIH_LX_USER:-newlevel}"
  STRIH_LX_PREP_PW="${STRIH_LX_PW:-newlevel}"
  STRIH_LX_BOX_HOST=(); STRIH_LX_BOX_GUARD=(); STRIH_LX_BOX_HOSTNAME=()
  for b in "${box_list[@]}"; do
    STRIH_LX_BOX="$b"
    host="$(fleet_box_ip "$b")" || { _strih_lx_fail resolve 2 3 "no ${b} host (STRIH_LX_IP / obs-fleet row)"; return; }
    strih_lx_dial_host_check "$b" "$host" || return
    # the rig-busy guard's inputs, resolved here so a missing one refuses before any box is touched:
    # the venue's strih + stream OBS hosts (strih_lx_guard_hosts: obs-fleet rows, the box's fact file).
    guard="$(strih_lx_guard_hosts "$b" "$host")" \
      || { _strih_lx_fail resolve 2 3 "no stream host (obs-fleet row) for the rig-busy guard -- the venue strih/stream of ${b} (above)"; return; }
    STRIH_LX_BOX_HOST[$b]="$host"; STRIH_LX_BOX_GUARD[$b]="$guard"
  done
  STRIH_LX_BOX="$boxes"
  STRIH_LX_PREP_HOST="${STRIH_LX_BOX_HOST[${box_list[0]}]}"
  STRIH_LX_PREP_OBS_PHASE2_DIR="$(strih_lx_obs_phase2_dir)" && [ -f "$STRIH_LX_PREP_OBS_PHASE2_DIR/obs_phase2.py" ] \
    || { _strih_lx_fail resolve 2 3 "no obs_phase2.py in '${STRIH_LX_PREP_OBS_PHASE2_DIR:-}' -- the rig-busy guard could not read strih/stream"; return; }
  declare -F stray_session_check_assert >/dev/null && declare -F stray_session_busy_summary >/dev/null \
    || { _strih_lx_fail resolve 2 3 "stray_session_check_assert / stray_session_busy_summary are not loaded (source scripts/lib/stray-session-check.sh) -- the deploy never stops OBS unguarded"; return; }
  STRIH_LX_PREP_STAGE="$(strih_lx_stage_dir "$sha")" || { _strih_lx_fail resolve 2 3 "canonical SHA '$sha' is not a hex commit id"; return; }
  local vp="${STRIH_LX_VERIFY_POLLS:-24}" vs="${STRIH_LX_VERIFY_POLL_SECS:-10}" st="${STRIH_LX_VERIFY_SETTLE_SECS:-90}"
  # the first poll has no sleep before it, so the observable window is (polls - 1) x secs.
  if [ "$st" -gt 0 ] && [ $(((vp - 1) * vs)) -lt "$st" ]; then
    _strih_lx_fail resolve 2 3 "STRIH_LX_VERIFY_SETTLE_SECS=$st cannot fit the read-back window (${vp} - 1) x ${vs} s -- the deploy would always be refused"; return
  fi
  for tool in sshpass rsync curl tar jq timeout python3; do
    command -v "$tool" >/dev/null 2>&1 || { _strih_lx_fail resolve 127 3 "$tool is required for the ${STRIH_LX_BOX} deploy"; return; }
  done
  art="$(fleet_linux_bundle_artifact_for "${box_list[0]}")"
  lrun="$(gh run list --repo "$gh_repo" --workflow linux-genlock.yml --json databaseId,headSha,conclusion --limit 50 2>/dev/null | fleet_pick_run_at_sha "$sha")" || true
  [ -n "$lrun" ] || { _strih_lx_fail resolve 1 3 "no successful linux-genlock.yml run at $sha -- build the strih artifact at that commit first (a tag dispatch)"; return; }

  mkdir -p "$w/bundle" "$w/repo" || { _strih_lx_fail download 1 3 "cannot create $w"; return; }
  gh run download "$lrun" --repo "$gh_repo" -n "$art" -D "$w/bundle"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail download "$rc" 3 "download of $art (run $lrun) failed"; return; }
  [ -f "$w/bundle/bin/obs" ] || { _strih_lx_fail download 1 3 "$art has no bin/obs"; return; }
  [ -d "$w/bundle/lib/x86_64-linux-gnu/obs-plugins" ] || { _strih_lx_fail download 1 3 "$art has no lib/x86_64-linux-gnu/obs-plugins"; return; }
  art_sha="$(tr -d '[:space:]' 2>/dev/null < "$w/bundle/GENLOCK_BUILD_SHA.txt")"
  [ "$art_sha" = "$sha" ] || { _strih_lx_fail download 1 3 "$art (run $lrun) carries GENLOCK_BUILD_SHA ${art_sha:-<none>}, not the canonical $sha -- refusing to stage a foreign build"; return; }
  STRIH_LX_PREP_LIBSHA="$(jq -r '.files[]? | select(.path == "lib/x86_64-linux-gnu/libobs.so.30") | .sha256' "$w/bundle/BUNDLE_MANIFEST.json" 2>/dev/null | head -n 1)"
  [ -n "$STRIH_LX_PREP_LIBSHA" ] || { _strih_lx_fail download 1 3 "$art BUNDLE_MANIFEST.json has no lib/x86_64-linux-gnu/libobs.so.30 sha256 -- the read-back could not prove the installed bytes"; return; }
  echo "# ${STRIH_LX_BOX}: $art (run $lrun) downloaded, GENLOCK_BUILD_SHA=$art_sha"

  tree_rev="$(git -C "$repo" rev-parse --short HEAD 2>/dev/null)" || tree_rev=""
  [ -n "$tree_rev" ] || { _strih_lx_fail tree 1 3 "$repo is not a git checkout"; return; }
  local -a tree_paths; mapfile -t tree_paths < <(strih_lx_tree_paths)
  git -C "$repo" archive --format=tar HEAD "${tree_paths[@]}" | tar -x -C "$w/repo"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail tree "$rc" 3 "archiving ${tree_paths[*]} at $tree_rev failed"; return; }
  for b in "${box_list[@]}"; do
    STRIH_LX_BOX="$b"
    STRIH_LX_BOX_HOSTNAME[$b]="$(strih_lx_tree_check "$w/repo" "$b")" \
      || { _strih_lx_fail tree 1 3 "the provisioning tree at $tree_rev is incomplete (above)"; return; }
    # one runner per box (it runs `setup-strih.sh --box <box>`); apply copies it to repo/run-setup.sh.
    strih_lx_setup_runner "$STRIH_LX_PREP_STAGE" "$b" > "$w/run-setup.$b.sh" || { _strih_lx_fail tree 1 3 "cannot write run-setup.sh"; return; }
  done
  STRIH_LX_BOX="$boxes"
  STRIH_LX_PREP_TOKEN="${STRIH_LX_GH_TOKEN:-}"
  [ -n "$STRIH_LX_PREP_TOKEN" ] || STRIH_LX_PREP_TOKEN="$(gh auth token 2>/dev/null)" || STRIH_LX_PREP_TOKEN=""
  [ -n "$STRIH_LX_PREP_TOKEN" ] || { _strih_lx_fail tree 1 3 "no GH token (STRIH_LX_GH_TOKEN / gh auth token) -- setup-strih.sh needs GH_TOKEN for the bundle-state + bkshading fetch"; return; }
  for b in "${box_list[@]}"; do
    echo "# ${b}: provisioning tree = the committed ${tree_paths[*]} at $tree_rev; target ${STRIH_LX_PREP_USER}@${STRIH_LX_BOX_HOST[$b]}:$STRIH_LX_PREP_STAGE"
  done
}

# strih_lx_apply SHA [BOX] -> the box steps for BOX (default strih-snv; issue 1426: any box
# strih_lx_prepare resolved), after strih_lx_prepare (and every other box's resolution) succeeded.
# 0 only after the fail-closed read-back passed; 4 with a named step otherwise; 5 = installed and read
# back, acceptance not clear; 6 = AWAY: a `traveling` box whose ssh does not connect (rc 255, or the
# 124 bound) is skipped with one AWAY line, nothing changed -- never a failure (its venue is off).
strih_lx_apply() {
  local sha="$1" box="${2:-strih-snv}" w="$STRIH_LX_PREP_WORK" stage="$STRIH_LX_PREP_STAGE" rc out line
  STRIH_LX_BOX="$box"
  STRIH_LX_PREP_HOST="${STRIH_LX_BOX_HOST[$box]:-}"
  STRIH_LX_PREP_HOSTNAME="${STRIH_LX_BOX_HOSTNAME[$box]:-}"
  local guard="${STRIH_LX_BOX_GUARD[$box]:-}"
  STRIH_LX_PREP_GUARD_STRIH="${guard%% *}"
  STRIH_LX_PREP_STREAM_HOST="${guard#* }"
  [ -n "$STRIH_LX_PREP_HOST" ] && [ -n "$STRIH_LX_PREP_HOSTNAME" ] && [ -n "$guard" ] \
    || { _strih_lx_fail preflight 2 4 "${box} was not resolved by strih_lx_prepare -- nothing changed"; return; }
  cp -f "$w/run-setup.$box.sh" "$w/repo/run-setup.sh" \
    || { _strih_lx_fail preflight 1 4 "cannot stage the run-setup.sh of ${box} -- nothing changed"; return; }

  # [preflight] the right box (a dial override must not provision another box as ${box}) ...
  out="$(_strih_lx_ssh "$(strih_lx_remote_identity_cmd)")"; rc=$?
  if [ "$rc" != 0 ] && { [ "$rc" = 255 ] || [ "$rc" = 124 ]; } && [ "$(obs_fleet_home_check "$box" 2>/dev/null)" = traveling ]; then
    echo "AWAY: [${box}] ${STRIH_LX_PREP_HOST} does not answer over ssh (rc=$rc; a traveling box, its venue is off) -- skipped, nothing changed; deploy it with --boxes ${box} once it is back"
    return 6
  fi
  [ "$rc" = 0 ] || { _strih_lx_fail preflight "$rc" 4 "cannot reach ${STRIH_LX_PREP_HOST} over ssh -- nothing changed"; return; }
  out="$(printf '%s' "$out" | tr -d '[:space:]')"
  # issue 1414: a box that still runs the pre-rename hostname of THIS box (exactly the old name in the
  # one transition alias table) IS this box -- setup-strih step 1 renames it from the fact file.
  if [ "$out" != "$STRIH_LX_PREP_HOSTNAME" ] && [ "$out" = "${out,,}" ] \
     && [ "$(_obs_fleet_alias_of "$out")" = "$STRIH_LX_PREP_HOSTNAME" ]; then
    echo "WARNING: [${box} preflight] the box at ${STRIH_LX_PREP_HOST} still runs its pre-rename hostname '${out}' (issue 1414) -- deploying; setup-strih renames it to '${STRIH_LX_PREP_HOSTNAME}'" >&2
    out="$STRIH_LX_PREP_HOSTNAME"
  fi
  [ "$out" = "$STRIH_LX_PREP_HOSTNAME" ] || { _strih_lx_fail preflight 1 4 "the box at ${STRIH_LX_PREP_HOST} is '${out:-<none>}', not '${STRIH_LX_PREP_HOSTNAME}' (scripts/strih-boxes/${box}.env) -- refusing to provision it as ${box}; nothing changed"; return; }
  # ... and never race a previous install (the sweep would delete the stage it is reading).
  out="$(_strih_lx_ssh "$(strih_lx_remote_installer_cmd)")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail preflight "$rc" 4 "cannot reach ${STRIH_LX_PREP_HOST} over ssh -- nothing changed"; return; }
  [ "$(printf '%s' "$out" | tr -d '[:space:]')" = "idle" ] || { _strih_lx_fail preflight 1 4 "a previous setup-strih.sh is still running on the box (pgrep -x setup-strih.sh) -- wait for it; nothing changed"; return; }
  # ... and never while a broadcast is LIVE: this deploy STOPS the production strih OBS. The shared
  # rig-busy guard, immediately before the first mutation (and again right before the stop, below).
  _strih_lx_broadcast_guard preflight "nothing changed" || return

  # [sweep] stage created + touched newest FIRST, then the retention sweep keeps only it.
  _strih_lx_ssh "$(strih_lx_remote_prep_cmd "$stage")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail sweep "$rc" 4 "creating $stage on the box failed -- nothing staged, nothing stopped"; return; }
  _strih_lx_ssh "$(strih_lx_remote_sweep_cmd)" < "$w/repo/scripts/obs-backup-retention.sh"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail sweep "$rc" 4 "the obs-backup-retention --local-sweep of stale /tmp/genlock-stage-* failed -- nothing staged, nothing stopped"; return; }
  out="$(_strih_lx_ssh "$(strih_lx_remote_stage_check_cmd "$stage")")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail sweep "$rc" 4 "$stage is not intact after the sweep -- refusing to continue"; return; }
  while IFS= read -r line; do
    case "$line" in LEFTOVER\ *) echo "WARNING: [${box} sweep] could not remove ${line#LEFTOVER } -- it still counts against the /tmp quota" >&2 ;; esac
  done <<< "$out"

  # [stage] every byte on the box BEFORE anything is stopped.
  _strih_lx_rsync "$w/bundle/" "$stage/bundle/"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail stage "$rc" 4 "rsync of the bundle to $stage/bundle failed (rc 11/23 = the /tmp quota) -- OBS NOT stopped, the old build keeps running"; return; }
  _strih_lx_rsync "$w/repo/" "$stage/repo/"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail stage "$rc" 4 "rsync of the provisioning tree to $stage/repo failed -- OBS NOT stopped, the old build keeps running"; return; }
  echo "# ${box}: staged $stage/{bundle,repo}"

  # [stop] the guard again first: staging can take minutes, and one early check is not enough
  # (.claude/rules/rig-mutation-broadcast-guard.md). Then delegated to the sanctioned stop code.
  _strih_lx_broadcast_guard stop "OBS NOT stopped, the old build keeps running ($stage stays for the next sweep)" || return
  _strih_lx_ssh "$(strih_lx_remote_stop_cmd)"; rc=$?
  if [ "$rc" != 0 ]; then
    _strih_lx_start_best_effort
    _strih_lx_fail stop "$rc" 4 "the graceful strih-obs stop did not complete -- nothing installed"; return
  fi

  # [setup] detached as root; the token rides the launch's stdin only.
  _strih_lx_ssh "$(strih_lx_remote_setup_launch_cmd "$stage")" < <(printf '%s\n' "$STRIH_LX_PREP_PW"; printf 'ghtoken:%s\n' "$STRIH_LX_PREP_TOKEN"); rc=$?
  if [ "$rc" != 0 ]; then
    # The preflight saw no installer, so one running now is THIS deploy's (the ssh dropped after the
    # detach -- exactly what the detach is for): follow it through the rc poll below.
    case "$(_strih_lx_installer_state)" in
      idle)
        _strih_lx_start_best_effort
        _strih_lx_fail setup "$rc" 4 "launching setup-strih.sh failed (sudo / run-setup.sh) -- nothing installed"; return ;;
      unreachable)
        _strih_lx_fail setup "$rc" 4 "the launch ssh returned rc=$rc and the box is unreachable -- the installer state is unknown; NOT starting OBS: check $stage/setup-strih.rc once the box answers"; return ;;
    esac
    echo "WARNING: [${box} setup] the launch ssh returned rc=$rc but setup-strih.sh is running -- following it through the rc poll" >&2
  fi
  local polls="${STRIH_LX_SETUP_POLLS:-270}" secs="${STRIH_LX_SETUP_POLL_SECS:-10}" i=0 src=""
  while [ "$i" -lt "$polls" ]; do
    src="$(_strih_lx_ssh "$(strih_lx_remote_setup_rc_cmd "$stage")" 2>/dev/null | tr -d '[:space:]')"
    [ -n "$src" ] && break
    i=$((i + 1)); sleep "$secs"
  done
  if [ -z "$src" ]; then
    if [ "$(_strih_lx_installer_state)" != idle ]; then
      _strih_lx_fail setup 124 4 "setup-strih.sh wrote no rc after $polls polls x ${secs} s and is still running (or the box is unreachable) -- NOT starting OBS over it; wait for $stage/setup-strih.rc"; return
    fi
    echo "# ${box}: setup-strih.log tail:"; _strih_lx_ssh "$(strih_lx_remote_setup_log_cmd "$stage")" || true
    _strih_lx_start_best_effort
    _strih_lx_fail setup 124 4 "setup-strih.sh wrote no rc and is no longer running (killed?)"; return
  fi
  local accept=0 gate
  if [ "$src" = 1 ]; then
    gate="$(_strih_lx_ssh "$(strih_lx_remote_setup_gate_cmd "$stage")" 2>/dev/null | tr -d '[:space:]')"
    if [ "$gate" = 1 ]; then
      # every install step passed; only step 17's verify-strih.sh failed because OBS is stopped by
      # this deploy -- the real acceptance gate runs below, once OBS is up and read back.
      accept=1; src=0
      echo "# ${box}: setup-strih.sh installed everything; its own final gate ran with OBS stopped -- the acceptance gate runs after the start"
    fi
  fi
  if [ "$src" != 0 ]; then
    echo "# ${box}: setup-strih.log tail:"; _strih_lx_ssh "$(strih_lx_remote_setup_log_cmd "$stage")" || true
    _strih_lx_start_best_effort
    _strih_lx_fail setup "$src" 4 "setup-strih.sh exited rc=$src (log above, $stage/setup-strih.log) -- the deploy is FAILED"; return
  fi
  echo "# ${box}: setup-strih.sh done"
  out="$(_strih_lx_ssh "$(strih_lx_remote_setup_notes_cmd "$stage")" 2>/dev/null)" || out=""
  while IFS= read -r line; do
    [ -n "$line" ] && echo "# ${box}: NOTE from setup-strih.sh: $line"
  done <<< "$out"

  # [start]
  _strih_lx_ssh "$(strih_lx_remote_start_cmd "$stage" "$box")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail start "$rc" 4 "systemctl --user start strih-obs.service failed"; return; }

  _strih_lx_verify "$sha" || return
  if [ "$accept" = 1 ]; then
    # [accept] the acceptance gate setup-strih.sh could not run meaningfully with OBS stopped. A
    # failure here is exit 5, NOT 4: the new build is installed, running and read back; only the
    # whole-box gate (which also grades things the deploy cannot change) is not clear.
    out="$(_strih_lx_ssh "$(strih_lx_remote_accept_cmd "$stage" "$box")" < <(printf '%s\n' "$STRIH_LX_PREP_PW"))"; rc=$?
    printf '%s\n' "$out"
    if [ "$rc" != 0 ]; then
      _strih_lx_fail accept "$rc" 5 "the new build is installed + running (read back), but verify-strih.sh --box ${box} did not pass: $(printf '%s\n' "$out" | grep -F 'FAIL' | sed 's/\x1b\[[0-9;]*m//g; s/^[[:space:]]*//' | tr '\n' ';')"
      return
    fi
    echo "# ${box}: acceptance gate (verify-strih.sh --box ${box}) passed"
  fi
}

# strih_lx_apply_all SHA LX_BOXES OTHER_BOXES RUN MODE LOG -> strih_lx_apply for each setup-strih box of
# the comma list LX_BOXES, in order (issue 1426), the fleet-log line of each written the moment it is
# known (deploy-genlock-fleet.sh's fleet_log_line): VERIFIED = `<box>`, acceptance not clear =
# `<box>:accept-failed`, AWAY = no line. A failed acceptance stops only that box (the next box still
# deploys) and the call returns 5 naming it, with OTHER_BOXES (the Windows / imag boxes the planner would
# do next) WITHHELD; an AWAY box never fails the run; any other failure returns at once (4). The GH
# token is cleared once every box has launched its setup.
strih_lx_apply_all() {
  local sha="$1" lx="$2" others="$3" run="$4" mode="$5" log="$6" b rc accept_failed="" away=""
  local -a list
  IFS=',' read -r -a list <<< "$lx"
  mkdir -p "$(dirname "$log")"
  for b in "${list[@]}"; do
    rc=0
    strih_lx_apply "$sha" "$b" || rc=$?
    case "$rc" in
      0) fleet_log_line "$run" "$sha" "$b" "$mode" >> "$log" ;;   # logged at once, a later failure never hides it
      5) fleet_log_line "$run" "$sha" "$b:accept-failed" "$mode" >> "$log"; accept_failed="${accept_failed:+$accept_failed,}$b" ;;
      6) away="${away:+$away,}$b" ;;
      *) STRIH_LX_PREP_TOKEN=""; return "$rc" ;;
    esac
  done
  STRIH_LX_PREP_TOKEN=""
  [ -z "$away" ] || echo "# AWAY (skipped, nothing changed): ${away} -- deploy at $sha with --boxes ${away} once back"
  [ -n "$accept_failed" ] || return 0
  [ -z "$others" ] || echo "NOTE: the other boxes (${others}) were WITHHELD -- deploy them at $sha once the acceptance items are understood, or the fleet stays on two builds" >&2
  [ "$accept_failed" = "$lx" ] || echo "NOTE: the acceptance gate is not clear on ${accept_failed} (installed + running)" >&2
  return 5
}

# [verify] fail-closed: every field must pass, and the SAME non-zero MainPID + NRestarts must hold from
# the first good poll for STRIH_LX_VERIFY_SETTLE_SECS (default 90 -- a crash loop can be slower than
# one poll interval: the CEF trap on this box fired every ~60 s). A bad poll restarts the window.
# Bash SECONDS follows the wall clock: dev1 is dantesync-disciplined (slews, a step is itself an
# alarm), and the settle is a lower bound on observed stability, so a step can only shift it by the
# step size -- accepted rather than a /proc/uptime dependency.
_strih_lx_verify() {
  local sha="$1" vpolls="${STRIH_LX_VERIFY_POLLS:-24}" vsecs="${STRIH_LX_VERIFY_POLL_SECS:-10}"
  local settle="${STRIH_LX_VERIFY_SETTLE_SECS:-90}"
  local i=0 have_first=0 first_pid="" first_r="" first_t=0 line pid restarts bs verdict="FAIL: no read-back" url
  url="$(strih_lx_bundle_state_url "$STRIH_LX_PREP_HOST")"
  while [ "$i" -lt "$vpolls" ]; do
    [ "$i" -gt 0 ] && sleep "$vsecs"
    i=$((i + 1))
    line="$(_strih_lx_ssh "$(strih_lx_remote_readback_cmd "$STRIH_LX_PREP_STAGE")" 2>/dev/null)" || line=""
    pid="$(_strih_lx_field pid "$line")"; restarts="$(_strih_lx_field restarts "$line")"
    bs="$(curl -s -m 8 "$url" 2>/dev/null | jq -r '.genlock_build_sha // empty' 2>/dev/null)" || bs=""
    if ! verdict="$(strih_lx_deploy_verdict "$sha" "$(_strih_lx_field installed "$line")" "$(_strih_lx_field active "$line")" "$bs" "$STRIH_LX_PREP_LIBSHA" "$(_strih_lx_field lib "$line")" "$(_strih_lx_field tick "$line")")"; then
      have_first=0; continue
    fi
    if [ "$have_first" = 1 ]; then
      if verdict="$(strih_lx_stable_verdict "$first_pid" "$first_r" "$pid" "$restarts")"; then
        if [ $((SECONDS - first_t)) -ge "$settle" ]; then
          echo "# ${STRIH_LX_BOX}: VERIFIED -- marker + libobs bytes = $sha, strih-obs active (MainPID $pid stable for $((SECONDS - first_t)) s, NRestarts $restarts), render tick ENABLED, :8899=$bs"
          return 0
        fi
        verdict="FAIL: MainPID $pid stable for $((SECONDS - first_t)) s only (settle ${settle} s)"
        continue
      fi
    else
      verdict="FAIL: only one good read-back -- the MainPID/NRestarts stability needs a later poll"
    fi
    have_first=1; first_pid="$pid"; first_r="$restarts"; first_t="$SECONDS"
  done
  _strih_lx_fail verify 1 4 "post-deploy read-back refused after $vpolls polls: $(printf '%s' "$verdict" | tr '\n' ';') (see $url)"
}

# --- plan arm -------------------------------------------------------------------------------------

# strih_lx_plan_steps LOCAL_STAGE SHA HOST RUN_ID [BOX] -> the BOX part of `--plan` (default strih-snv;
# issue 1426: any setup-strih box, e.g. strih-pp at strih-pp.lan): the SAME remote commands the execute
# arm runs (built by the builders above), every line a #-comment so a saved whole-plan .ps1 still parses
# (the issue-1295 file-mode rule). A traveling box's plan states the AWAY skip. Pure.
strih_lx_plan_steps() {
  local lstage="$1" sha="$2" host="$3" run="$4" box="${5:-strih-snv}" stage art guard gstrih away=""
  local u="${STRIH_LX_USER:-newlevel}"
  stage="$(strih_lx_stage_dir "$sha")" || return 2
  art="$(fleet_linux_bundle_artifact_for "$box")"
  guard="$(strih_lx_guard_hosts "$box" "$host")" || { echo "strih_lx_plan_steps: no venue strih/stream host for ${box} (fact file STRIH_VENUE / obs-fleet rows)" >&2; return 2; }
  gstrih="${guard%% *}"
  if [ "$(obs_fleet_home_check "$box" 2>/dev/null)" = traveling ]; then
    away=$'\n'"#   ${box} is a TRAVELING box: when ${host} does not answer over ssh it is reported AWAY and skipped
#   (nothing changed, the other boxes go on) -- never a failure; deploy it again once it is back."
  fi
  cat <<PLAN
# EXECUTE mode runs every step below itself, fail-loud (a failed step exits 4 and names itself; the
#   old build keeps running until the whole stage is on the box; every requested box is resolved
#   before any box is changed):
#     scripts/deploy-genlock-fleet.sh --run-id ${run} --boxes ${box}
#   ${box} always gets the strih FULL build (${art}); --fast is Windows-only.${away}
#   Every ssh below is 'sshpass -p \$PW timeout 180 ssh ${STRIH_LX_SSH_OPTS} ${u}@${host} <cmd>'.
# STEP 0 (resolve): the linux-genlock.yml run at ${sha}; download its '${art}'
#          artifact to ${lstage}/bundle -- REFUSED unless its GENLOCK_BUILD_SHA.txt is ${sha} and its
#          BUNDLE_MANIFEST.json carries lib/x86_64-linux-gnu/libobs.so.30. The planner adds
#          bundle/GENLOCK_STATS_ABI.txt = the stats + output-stats versions of obs.h at ${sha} (removed
#          when either is unreadable; issue 1302), which setup-strih.sh records next to GENLOCK_BUILD_SHA.txt.
# STEP 1 (tree): the committed scripts/ systemd/ intercom/ vendor/realtek-r8152 + the program-audio sampler's vendored
#          dock headers (strih_lx_tree_paths, issue 1404) of this checkout (archive of HEAD) + the
#          generated run-setup.sh -> ${lstage}/repo.
# STEP 2 (preflight): the box must be the fact file's host ('$(strih_lx_remote_identity_cmd)' == STRIH_HOSTNAME),
#          and no previous install may run:  $(strih_lx_remote_installer_cmd)
#          and no broadcast may be LIVE: the shared rig-busy guard stray_session_check_assert
#          (scripts/lib/stray-session-check.sh) reads strih ${gstrih} + the obs-fleet stream host --
#          streaming/recording on either = exit 4, nothing changed (re-checked right before STEP 5).
# STEP 3 (sweep): the stage is created + touched NEWEST, then the obs-backup-retention.sh
#          --local-sweep decision (what 'obs-backup-retention.sh --box ${box}' runs), stage dirs
#          only, as the operator, keeps only it:
#            $(strih_lx_remote_prep_cmd "$stage")
#            $(strih_lx_remote_sweep_cmd)   < scripts/obs-backup-retention.sh (the committed tree)
#            $(strih_lx_remote_stage_check_cmd "$stage")
# STEP 4 (stage, OBS still running):
#            rsync -a --delete --timeout=120 -e "ssh ${STRIH_LX_SSH_OPTS}" ${lstage}/bundle/ ${u}@${host}:${stage}/bundle/
#            rsync -a --delete --timeout=120 -e "ssh ${STRIH_LX_SSH_OPTS}" ${lstage}/repo/ ${u}@${host}:${stage}/repo/
# STEP 5 (stop, delegated to the sanctioned stop code; the deploy sends no kill itself):
#            $(strih_lx_remote_stop_cmd)
# STEP 6 (setup, detached on the box): 'setup-strih.sh --box ${box}' as root with
#          STRIH_LX_BUNDLE_SRC=${stage}/bundle and GH_TOKEN read from stdin (a 'ghtoken:<token>' line, never argv):
#            $(strih_lx_remote_setup_launch_cmd "$stage")   < (the sudo password line + ghtoken:<token>)
#          then poll: $(strih_lx_remote_setup_rc_cmd "$stage")  (0 = done; else the log tail + FAIL;
#          OBS is never started while setup-strih.sh still runs)
# STEP 7 (start): $(strih_lx_remote_start_cmd "$stage" "$box")
# STEP 8 (verify, fail-closed, held for ${STRIH_LX_VERIFY_SETTLE_SECS:-90} s): /opt/obs-genlock/GENLOCK_BUILD_SHA.txt == ${sha},
#          /usr/lib/x86_64-linux-gnu/libobs.so.30 sha256 == the manifest's, strih-obs.service active with
#          the SAME MainPID + NRestarts, 'render tick ENABLED' in the OBS log written after the start,
#          and $(strih_lx_bundle_state_url "$host") genlock_build_sha == ${sha}.
# STEP 9 (accept): setup-strih.sh's own final gate runs with OBS stopped; when that gate is its ONLY
#          failure ($(strih_lx_remote_setup_gate_cmd "$stage")), execute runs the gate itself now:
#            $(strih_lx_remote_accept_cmd "$stage" "$box")   < (the sudo password line)
#          (a reboot-pending run only reports -- run verify-strih.sh --box ${box} after the reboot).
# ACCEPTANCE (supervisor, after a green execute): from dev1
#          'python3 scripts/obs_burn_filter.py check --host ${host}' (the WS filter-enum survives).
PLAN
}
