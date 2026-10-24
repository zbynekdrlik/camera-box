---
paths:
  - "scripts/lib/stream-box.sh"
  - "scripts/lib/audio-presence-preflight.sh"
  - "scripts/lib/marker-decodability-preflight.sh"
  - "scripts/lib/bundle-state-selfheal.sh"
  - "scripts/lib/shell-quote.sh"
  - "scripts/lib/stream-lx-verdict.sh"
  - "scripts/recording-verdict-on-stream-snv.sh"
  - "tests/python/*1462*"
---

# The stream box is Linux (stream-snv) — every dev1 step that runs ON it picks the transport by fleet class (issue 1462)

On 22.10.2026 the Windows stream PC (10.77.9.204) was reinstalled as the Linux **stream-snv**: the
stream role of setup-strih.sh, the same OBS appliance as stream-pp (obs-fleet row
`stream-snv|10.77.9.204|linux-genlock|always`, facts `scripts/strih-boxes/stream-snv.env`). Every dev1
step that sent PowerShell to it through `win_ssh_run` failed there (rig-mode TEST exited 1).

## The ONE place: `scripts/lib/stream-box.sh` (source-only, no strict mode)

| Function | What it does |
|---|---|
| `stream_box_platform HOST` | `linux` iff `obs_fleet_class_for_host` reads `linux-genlock`, else `windows` (unknown / empty host too). Never a literal IP. |
| `stream_box_exec USER PW HOST WIN_PS LINUX_SH` | Linux: `sshpass ssh -n` with the bash text as ONE remote argument (an EMPTY bash text is refused, rc 2: `ssh host ""` opens an interactive shell). Windows: `win_ssh_run` with the PowerShell text, byte-identical to before. rc = the remote command's. |
| `stream_box_download USER PW HOST WIN_REMOTE LINUX_REMOTE LOCAL` | Linux: `scp`; Windows: `win_ssh_download`. |
| `stream_box_visibility_gate_linux HOST USER PW TIMEOUT HERE` | the `[0/8]` operator-visibility gate on a Linux stream box = `strih_linux_visibility_check` (the box runs the same `strih-obs.service` as strih-snv), told the box's fleet name, so an unreachable box reads `the stream box stream-snv unreachable`, never strih-snv; ERROR + the `systemctl --user restart strih-obs.service` recovery on stderr, rc 1. |
| `stream_mbc_vban_expect HOST` | Linux: `<stream> <port> <input>` from the box facts (`STRIH_PROGRAM_VBAN_STREAMS` / `_PORT` / `STRIH_PROGRAM_AUDIO_INPUT`, loaded in a SUBSHELL, so the caller never gets a loaded box); the input name may hold spaces (stream-pp `Zvuk z mixu`), so it comes last: `read -r stream port input`. Windows: nothing, rc 0. A Linux box refuses (rc 1) with one of two named messages: no fleet row / the facts did not load (the loader's own line says why), or the facts declare `none`. |

Argument order follows the wrapped function: exec / download take win_ssh_run's (USER PW HOST ...), the
visibility gate strih_linux_visibility_check's (HOST USER PW TIMEOUT HERE). Reordering the gate would
also put `"$STREAM_USER" "$STREAM_PW" "$STREAM"` into the `[0/8]` Linux branch ahead of the Windows
leg, the literal `harness_obs_session_visibility_977.rs` finds the Windows call's arguments by.

- **Callers hand BOTH texts.** The PowerShell builders are unchanged; each has a bash twin next to it:
  `audio_preflight_volumedetect_sh` / `audio_preflight_delete_sh`, `marker_decodability_extract_wav_sh` /
  `marker_decodability_delete_sh`. A path is single-quoted for the box's shell by the ONE helper
  `shell_single_quote` (`scripts/lib/shell-quote.sh`, lazy-sourced by each builder; `'` -> `'\''`):
  an OBS recording name carries a space (`YYYY-MM-DD HH-MM-SS.mkv`, in the user's home on stream-snv:
  the profile sets no record path). `strih_lx_recording_cleanup_note` uses it too; the rig-test
  ledger's private `_rig_test_ledger_shell_quote` and `vban-receiver-bench.sh` keep their own copies.
- **`UserKnownHostsFile=/dev/null` on the Linux transport.** dev1's `~/.ssh/known_hosts` still held the
  Windows box's keys for 10.77.9.204 (23.10.2026); with `StrictHostKeyChecking=no` OpenSSH then
  DISABLES password auth (the strih-snv log reader's rule). The two bundle-state restarts that now
  reach the Linux stream box (the E2E self-heal `bundle_state_selfheal_fetch` and the dev1
  `bundle-state-alert-watchdog.sh` `restart_bundle_state_task`) carry it too. `win_ssh_run` does not:
  it only ever talks to a Windows box. Any other dev1 tool that plain-ssh's 10.77.9.204 without the
  option reads it as down until the stale key goes (`ssh-keygen -R 10.77.9.204`, a supervisor step on
  dev1, not done by the lane).
- **Never source `win-ssh-exec.sh` from the lib** (its top-level `set -euo pipefail` would leak): the
  callers source it; a Windows box with no `win_ssh_run` defined is a named rc 127.
- **A `timeout`-bounded call re-sources BOTH libs** in its `bash -c` (timeout cannot exec a function):
  `bash -c '. "$1"; . "$2"; stream_box_exec "$3" "$4" "$5" "$6" "$7"' _ "$HERE/lib/win-ssh-exec.sh" "$HERE/lib/stream-box.sh" ...`.

## The consumers (ported in issue 1462)

- rig-mode TEST `#901 item 2` (`verify_mbc_dante_transport`): on a Linux stream box it passes
  `--vban-stream S --vban-port P` to `obs_phase2.py mbc-input-check`, which then grades the obs-vban
  RECEIVE source (the pure `program_audio_vban.mbc_vban_transport_problem`, next to the source's one
  definition: mute first, then the kind `VBAN_SOURCE_KIND`, then `settings_differences` for the stream
  + port, so an absent key is the source's default). The Receive Buffer is not graded there (verify
  item 9 NOTEs it). `MBC_INPUT_NAME` (default `mbc`) must equal the facts' `STRIH_PROGRAM_AUDIO_INPUT`,
  else TEST refuses before the read. Without the two flags the Dante device check is byte-identical.
- rig-mode TEST `#901 item 1` + recording-e2e `[4b2/8]`: the volumedetect + delete through
  `stream_box_exec`. recording-e2e `[4b3/8]`: the WAV extract + scp pull-back + delete; the box-side WAV
  is `AUDIO_DECODABILITY_WAV_LX` (default `camera-box-mbc-decodability.wav`, a RELATIVE path = the ssh
  user's home for both ssh and scp, never a fixed name in the world-writable `/tmp`). The bash extract
  removes that WAV before ffmpeg runs, and on a Linux box the WAV is pulled back only when the extract
  returned 0 (its rc is ffmpeg's; an ssh that never ran it returns non-zero), so neither a failed
  ffmpeg nor a failed ssh lets an aborted earlier run's WAV be probed as this run's audio. A Windows
  box keeps the old unconditional pull-back (its PowerShell rc carries no such signal).
- recording-e2e `[0/8]` session-visibility: a Linux stream box gets `stream_box_visibility_gate_linux`;
  the Windows leg stays byte-identical inside the `else` (pinned by `harness_strih_platform_1351.rs`).
  Mind the windows of `harness_obs_session_visibility_977.rs` from the gate banner (`GATE_WINDOW`,
  3600 bytes since issue 1462; the last anchor ends at byte 3092): the Linux branch is two lines.
- recording-e2e `[0/8]` dantesync version gate: a Linux stream box moves from `--win` to `--linux`
  (`DV_WIN_NODES` loses its `stream=` word), never in both arms.
- The bundle-state self-heal (`bundle_state_selfheal_class`): a non-strih host takes its OWN fleet
  class, so the Linux stream box gets the `systemctl --user` restart; `STRIH_PLATFORM` never applies
  to it.

## The full E2E on the Linux stream box -- the A/V measurement stays BLOCKING (issue 1462 lane 2)

Owner ROZHODNUTÉ 6377910161 (24.10.2026): "nikdy to nesmie byt skipped" -- the QR + audio-marker A/V
measurement (`recording-verdict --av-sync` data, carried in the stream partial) is part of every
full E2E, on a Linux stream box exactly as on Windows.

- **`[0/8]` version-integrity gate:** `version-integrity-gate.sh --linux NAME` (repeatable;
  `--strih-linux` = `--linux strih`) Linux-grades a `--win-state` box: the Windows-only rows
  (ndi_runtime, distroav_dll_paths, the obs.dll / distroav.dll byte rows, the #826 OBS-identity set)
  are loud SKIPPED rows naming the box, the rest is graded, a drift still refuses. recording-e2e.sh
  sets `STREAM_LINUX_GATE_ARG` from `stream_box_platform "$STREAM"` and passes
  `${STREAM_LINUX_GATE_ARG:+--linux stream}` in BOTH gate invocations, BEFORE the
  `${STRIH_LINUX_GATE_ARG:+--strih-linux}` tail (two tests pin that tail as the last argument).
  Details: `.claude/rules/version-integrity-gate.md` (issue 1462 section).
- **`[8/8b-pre]` pushes:** on a Linux stream box the marker log and the switch schedule go to
  `STREAM_LX_REMOTE_OUT_DIR` (default `/home/newlevel/verdict-out`) through
  `stream_lx_push_file USER PW HOST LOCAL DIR NAME` (`scripts/lib/stream-lx-verdict.sh`: mkdir + scp,
  prints the box path, rc 1 + a named line on any failure), in BOTH modes (the decode always runs
  for real there), and a failed push ENDS the run (`|| exit 1`): the decode reads them, a missing
  marker log would cost the A/V term. The Windows `if [ "$E2E_EXECUTE_VERDICT" = "1" ]` arm became
  an `elif` (its literal stays a substring, which the 703 test reads).
- **`[8/8b]` decode:** `run_stream_extract` takes a Linux branch first: `STREAM_LX_BOX="$STREAM"
  STREAM_USER=… STREAM_PW=… "$HERE/recording-verdict-on-stream-snv.sh"` with the same forwarded
  verdict args as Windows (`--av-marker-log`, `--switch-schedule`, the burn ids, `--cam2-run-id`, the
  CG flag, `$CG`) and `--out "$STREAM_LX_REMOTE_OUT_DIR/stream-partial-${RUN_ID}.json"`; the partial
  comes back to `$OUTDIR/stream-partial-<RUN_ID>.json` (= `$STREAM_PARTIAL`, the merge's `stream=`
  input, unchanged). The Windows call text below it is byte-identical.
- **`recording-verdict-on-stream-snv.sh` is an ENTRY POINT, never a copy:** it resolves the box
  (`STREAM_LX_BOX`, else `obs_fleet_host stream-snv`), refuses a non-linux-genlock box and an empty
  `--stream-rec` (exit 2, before any contact), then `exec`s `recording-verdict-on-strih-snv.sh` (the
  ONE Linux on-box decode: sha256 upload gate, `--skip-if-exists`, idle priority on the box's own
  E-cores -- stream-snv has `cpu_atom` 12-19 --, the partial + pixel dir pulled back unchanged) with
  `STRIH_LX_BOX`/`STRIH_USER`/`STRIH_PW` = the stream box, `ONBOX_LABEL=<fleet name>` (the log tag) and
  `--rec` (the decode's box-neutral alias of `--strih-rec`). A pinned test refuses a LOWPRIO /
  sha256 / scp copy in the wrapper.
- **A missing stream leg FAILS the run by name, never a skip:** a failed decode / unreachable box
  returns non-zero; in EXECUTE mode the #703 wait block exits 1 with `stream extract rc=N` or
  `a partial JSON is missing (… stream=…)`; in plan mode the foreground call aborts under `set -e`.
  The merge's `--merge-partials "stream=$STREAM_PARTIAL"` is unconditional, and the fused A/V gate
  folds into `all_pass` inside recording-verdict exactly as on Windows (no Linux-specific flag exists).
- The `[8/8b]` banner names the transport through `stream_access_label` (the `strih_access_label`
  twin): `stream-snv, plain ssh/scp` or `win-stream-snv`.
- Tests: `tests/python/test_stream_linux_e2e_1462.py` (the real gate over live-shaped states + the
  real gate block), `tests/python/test_stream_snv_decode_1462.py` (the wrapper RUN against the fake
  box, the lib, the push region and the #703 wait block cut out of recording-e2e.sh and run under
  `set -euo pipefail`). The fake `sshpass` (`stream_box_fakes_1462.py`) now also plays scp uploads,
  `scp -r` and a down box for scp; `link_box_tools` links real host tools into the box PATH.
- **Live acceptance (supervisor):** a full-path E2E `pull_request` run with stream-snv reaching `[8/8]`
  with a stream partial and a graded A/V term; the first run uploads `recording-verdict` to
  `/home/newlevel/recording-verdict` on stream-snv (the issue-1118 gate then skips it).

## Still Windows-only (follow-up candidates handed to the supervisor in the LANE-RETURN, not ported)

- The `[8/8e]` #652 cleanup plan line for the stream recording (`win-stream-snv Shell: Remove-Item`,
  three sites; plan text only, inside the 703 merge→exit window), and the missing-slot-pixels stream
  `.cmd` branch (`missing_slot_pixels_run` is handed the Linux path as `STREAM_REC_WIN`; report-only,
  `|| true`), and `strih_planner_holder_note`'s plan-mode text (it still says the MCP holder runs 8/8b).
- `scripts/av-soak.sh` (the stream decode kill + upload, `win_ssh_run` / `win_ssh_upload` at lines
  ~515 and ~953-956) and so `av-restart-matrix.sh`, which runs its windows through the soak.
- The Windows planners that still take `--box stream`: `launch-obs-genlock.sh` and
  `obs-self-heal-install.sh` (stream-snv restarts through `strih-obs.service`; both should refuse
  a linux-genlock box by class, `obs_fleet_refuse_linux_target`), and `avsync-watchdog-install.sh`
  (a Windows scheduled task on the stream box). `scripts/lib/genlock-fleet-boxes.sh` still maps
  stream-snv to the `win-stream-snv` MCP; the deploy planner already sends it the setup-strih arm.
- `scripts/lipsync-test-mode.sh`'s arrival verify (lines ~402-411: `win_ssh_download` of the probe
  recording and a `win_ssh_run` PowerShell delete against `$STREAM`); its two Rust harnesses copy only
  three libs into a temp dir, so a port must add `stream-box.sh` + `shell-quote.sh` to those copies.
- The opt-in restart / zero-loss modes' stream decode plans; the legacy `VERDICT_ON_STREAM=0`
  `recording-fetch-windows.sh` path; the obs-fleet `win-baseline` facet still lists stream-snv
  (report-only rows that read unread); the obs-session watchdog's class gate now skips stream-snv
  (its operator visibility is watched by nothing between E2E runs).

## Tests (Tier-0, no rig)

`tests/python/test_stream_box_lib_1462.py` (the lib, the builders RUN on a fake box),
`test_stream_box_consumers_1462.py` (rig-mode sourced; recording-e2e steps cut between count-1 anchors
and run under `set -euo pipefail`; Linux = the default fleet, Windows = an `OBS_FLEET` override that
keeps the PowerShell text), `test_mbc_vban_transport_1462.py` (the pure decision + the handler + the
CLI). Fakes: `tests/python/stream_box_fakes_1462.py` (a fake `sshpass` that RUNS the bash text on a
stub PATH in the box user's home `FAKE_BOX_HOME`, resolves a relative scp path there, plays a box
that does not answer with `FAKE_SSH_DOWN`, and decodes the PowerShell it would send).
