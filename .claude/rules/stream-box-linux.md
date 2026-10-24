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
  - "tests/python/*1474*"
  - "scripts/av-soak.sh"
  - "scripts/lipsync-test-mode.sh"
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
| `stream_box_upload USER PW HOST LOCAL WIN_REMOTE LINUX_REMOTE` | Linux: `scp` to the box; Windows: `win_ssh_upload` (issue 1462 lane 2, the `[8/8b-pre]` pushes). The remote dir must exist. |
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
  are loud SKIPPED rows naming the box, the rest is graded, a drift still refuses; since issue 1474
  the box's deployed bundle bytes are the fail-closed `linux_so_bytes` row instead. recording-e2e.sh
  sets `STREAM_LINUX_GATE_ARG` from `stream_box_platform "$STREAM"` and passes
  `${STREAM_LINUX_GATE_ARG:+--linux stream}` in BOTH gate invocations, BEFORE the
  `${STRIH_LINUX_GATE_ARG:+--strih-linux}` tail (two tests pin that tail as the last argument).
  Details: `.claude/rules/version-integrity-gate.md` (issue 1462 section).
- **`[8/8b-pre]` pushes:** on a Linux stream box the marker log and the switch schedule go to
  `STREAM_LX_REMOTE_OUT_DIR` (default `/home/newlevel/verdict-out`) through
  `stream_lx_push_file USER PW HOST LOCAL DIR NAME` (`scripts/lib/stream-lx-verdict.sh`: refuses a
  non-Linux box, a missing LOCAL, a NAME that is no plain file name and a DIR with whitespace / a quote
  before any contact, then `stream_box_exec` mkdir + `stream_box_upload` -- it opens no ssh of its own --,
  prints the box path, rc 1 + a named line on any failure), in BOTH modes (the decode always runs
  for real there), and a failed push ENDS the run (`|| exit 1`): the decode reads them, a missing
  marker log would cost the A/V term. The Windows `if [ "$E2E_EXECUTE_VERDICT" = "1" ]` arm became
  an `elif` (its literal stays a substring, which the 703 test reads).
- **`[8/8b]` decode:** `run_stream_extract` takes a Linux branch first: `STREAM_LX_BOX="$STREAM"
  STREAM_USER=… STREAM_PW=… "$HERE/recording-verdict-on-stream-snv.sh"` with the same forwarded
  verdict args as Windows (`--av-marker-log`, `--switch-schedule`, the burn ids, `--cam2-run-id`, the
  CG flag, `$CG`), `--remote-bin "$STREAM_LX_REMOTE_BIN"` (default `/home/newlevel/recording-verdict`)
  and `--out "$STREAM_LX_REMOTE_OUT_DIR/stream-partial-${RUN_ID}.json"`; the partial
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
- The `[8/8]` missing-slot pixel proofs of the stream recording (`scripts/lib/missing-slot-pixels.sh`,
  report-only) take the stream box's platform from its fleet class (`missing_slot_pixels_stream_os`):
  the Linux script at idle priority under `STREAM_LX_REMOTE_OUT_DIR` on stream-snv, the PowerShell
  `.cmd` export on a Windows / unknown host.
- Every `[8/8b]` site asks `stream_box_platform "$STREAM"` itself, never a cached variable: it is a
  pure fleet lookup on an IP literal (no I/O), the same per-site shape as the `strih_platform` calls,
  and one answer can never go stale against another inside the run.
- Tests: `tests/python/test_stream_linux_e2e_1462.py` (the real gate over live-shaped states + the
  real gate block), `tests/python/test_stream_snv_decode_1462.py` (the wrapper RUN against the fake
  box, the lib, the push region and the #703 wait block cut out of recording-e2e.sh and run under
  `set -euo pipefail`; `run_stream_extract`'s Linux branch is RUN, and the stream pixel export runs the
  REAL ffmpeg/ffprobe on the fake box). The fake `sshpass` (`stream_box_fakes_1462.py`) also plays scp
  uploads (REFUSED outside the test's own tmp dir: the "box" filesystem is this host, and a default
  `/home/newlevel/...` remote path once wrote a fake binary into dev1's home), `scp -r` and a down box
  for scp; `link_box_tools` links real host tools into the box PATH. Never run an `[8/8b]` path in a
  test without overriding every remote path (`STREAM_LX_REMOTE_OUT_DIR`, `STREAM_LX_REMOTE_BIN`).
- **Live acceptance (supervisor):** a full-path E2E `pull_request` run with stream-snv reaching `[8/8]`
  with a stream partial and a graded A/V term; the first run uploads `recording-verdict` to
  `/home/newlevel/recording-verdict` on stream-snv (the issue-1118 gate then skips it).

## Ported in issue 1474 (owner 24.10.2026: Linux coverage at least the Windows level)

- **The `[0/8]` byte rows:** the Linux boxes' deployed bundle bytes are graded again, the
  `linux_so_bytes` row (`.claude/rules/version-integrity-gate.md`, issue 1474 section).
- **The `[8/8e]` #652 cleanup plan** (all three sites): a `stream_box_platform` split whose Linux
  branch is `stream_lx_recording_cleanup_note` (`stream-lx-verdict.sh`), an exact-path `rm -f --`
  over plain ssh named by the box's fleet name; the Windows `Remove-Item` stays INLINE in the
  else-branch (`harness_recording_e2e_cleanup_after_decode.rs` pins it). The line itself is
  `shell_ssh_rm_plan_line` (`shell-quote.sh`), shared with `strih_lx_recording_cleanup_note`.
- **`scripts/av-soak.sh` (and through it `av-restart-matrix.sh`):** the soak resolves
  `STREAM_PLATFORM` once (`stream_box_platform`). On a Linux stream box: the marker log + schedule go
  up with `stream_lx_push_file` (in `lx_bounded`, a timeout-bounded child sourcing
  `stream-lx-verdict.sh`) into `STREAM_LX_OUT_DIR` (`AV_SOAK_STREAM_LX_OUT_DIR`, default
  `verdict-out`, relative = the ssh user's home), the decode is `recording-verdict-on-stream-snv.sh`
  with the `run_stream_extract` Linux shape (`av_soak_stream_lx_extract_argv`, the Linux
  `recording-verdict` from PROBE_BIN_DIR), a timed-out decode is stopped with
  `av_soak_lx_decode_kill_cmd stream` through `stream_box_exec`, and the cleanup plan's lines are the
  exact-path rm ones. No `WIN_VERDICT_EXE_LOCAL` is needed there (neither by the matrix). A Windows
  stream box keeps every PowerShell text: `tests/python/test_av_soak_orchestrator_1367.py` runs with
  an `OBS_FLEET` override marking 10.77.9.204 windows-genlock; the Linux path is
  `tests/python/test_av_soak_stream_lx_1474.py`. The `--plan` printout moved to
  `scripts/lib/av-soak-plan.sh` (the file budget).
- **`scripts/lipsync-test-mode.sh`'s arrival verify:** the probe pull is `stream_box_download` and the
  delete `stream_box_exec` with both texts (`audio_preflight_delete_ps` / `_sh`), each in a
  `timeout bash -c` that re-sources `win-ssh-exec.sh` + `stream-box.sh`. Its two Rust harnesses copy
  `stream-box.sh`, `shell-quote.sh` and `obs-fleet.sh` too (their STREAM `1.2.3.4` is no fleet row,
  so they keep exercising the Windows path); `tests/python/test_lipsync_arrival_stream_lx_1474.py`
  runs the Linux path (the delete proven by the probe file being gone).

- **Between E2E runs (issue 1474 item 3):** the obs-session watchdog's facet is strih-snv +
  stream-snv + resolume-snv and it picks the probe by class: a linux-genlock box gets the `[0/8]`
  Linux check (`strih-obs.service` over plain ssh + the bounded obs-websocket round-trip, split into
  `strih_linux_visibility_probe` / `strih_linux_visibility_ws_ok`), never PowerShell, and pages with
  the `systemctl --user restart strih-obs.service` recovery (`.claude/rules/obs-fleet-list.md`). The
  `win-baseline` facet keeps only its windows-genlock rows, so stream-snv is out of the Windows
  baseline (`tests/python/test_obs_session_linux_1474.py`).
- **The Windows planners (issue 1474 item 4):** `launch-obs-genlock.sh`, `obs-self-heal-install.sh`
  and `avsync-watchdog-install.sh` refuse a linux-genlock box through `obs_fleet_refuse_linux_target`
  (exit 2), `fleet_box_mcp` names no win-* MCP for a linux-genlock row, and the fleet deploy's execute
  arm prints no Windows paste-program for a box that took the setup-strih arm. Their suites run the
  Windows programs on a Windows-stream `OBS_FLEET` override
  (`tests/python/test_windows_planners_refuse_linux_1474.py`).

## Still Windows-only (follow-up candidates handed to the supervisor in the LANE-RETURN, not ported)

- `strih_planner_holder_note`'s plan-mode text (it still says the MCP holder runs 8/8b).
- The opt-in restart / zero-loss modes' stream decode plans; the legacy `VERDICT_ON_STREAM=0`
  `recording-fetch-windows.sh` path.
- Text that still names the Windows relaunch for the stream box, which now refuses on stream-snv:
  `rig-mode.sh`'s `print_genlock_relaunch_note` stream line (pinned by `tests/rig_mode.rs`
  `genlock_relaunch_note_is_env_free_no_mode`), `av-restart-matrix.sh`'s stream-obs SUPERVISOR step
  and its cleanup warning (on a Linux stream box that kind could restart `strih-obs.service` like
  the strih-obs kind, a matrix design call), and the `asio-starve` watchdog's default recovery plan
  (ships disabled; the VB-Matrix ASIO source is Windows-only).

## Tests (Tier-0, no rig)

`tests/python/test_stream_box_lib_1462.py` (the lib, the builders RUN on a fake box),
`test_stream_box_consumers_1462.py` (rig-mode sourced; recording-e2e steps cut between count-1 anchors
and run under `set -euo pipefail`; Linux = the default fleet, Windows = an `OBS_FLEET` override that
keeps the PowerShell text), `test_mbc_vban_transport_1462.py` (the pure decision + the handler + the
CLI). Fakes: `tests/python/stream_box_fakes_1462.py` (a fake `sshpass` that RUNS the bash text on a
stub PATH in the box user's home `FAKE_BOX_HOME`, resolves a relative scp path there, plays a box
that does not answer with `FAKE_SSH_DOWN`, and decodes the PowerShell it would send).
