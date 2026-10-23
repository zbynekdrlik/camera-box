---
paths:
  - "scripts/lib/stream-box.sh"
  - "scripts/lib/audio-presence-preflight.sh"
  - "scripts/lib/marker-decodability-preflight.sh"
  - "scripts/lib/bundle-state-selfheal.sh"
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
| `stream_box_exec USER PW HOST WIN_PS LINUX_SH` | Linux: `sshpass ssh` with the bash text as ONE remote argument. Windows: `win_ssh_run` with the PowerShell text, byte-identical to before. rc = the remote command's. |
| `stream_box_download USER PW HOST WIN_REMOTE LINUX_REMOTE LOCAL` | Linux: `scp`; Windows: `win_ssh_download`. |
| `stream_box_visibility_gate_linux HOST USER PW TIMEOUT HERE` | the `[0/8]` operator-visibility gate on a Linux stream box = `strih_linux_visibility_check` (the box runs the same `strih-obs.service` as strih-snv); ERROR + the `systemctl --user restart strih-obs.service` recovery on stderr, rc 1. |
| `stream_mbc_vban_expect HOST` | Linux: `<stream> <port>` from the box facts (`STRIH_PROGRAM_VBAN_STREAMS` / `_PORT`, loaded in a SUBSHELL, so the caller never gets a loaded box). Windows: nothing, rc 0. A Linux box with no fact file, or `none` facts, refuses (rc 1, named). |

- **Callers hand BOTH texts.** The PowerShell builders are unchanged; each has a bash twin next to it:
  `audio_preflight_volumedetect_sh` / `audio_preflight_delete_sh`, `marker_decodability_extract_wav_sh` /
  `marker_decodability_delete_sh`. A path is single-quoted for the box's shell (`'` -> `'\''`): an OBS
  recording name carries a space (`YYYY-MM-DD HH-MM-SS.mkv`, in the user's home on stream-snv: the
  profile sets no record path).
- **`UserKnownHostsFile=/dev/null` on the Linux transport.** dev1's `~/.ssh/known_hosts` still held the
  Windows box's keys for 10.77.9.204 (23.10.2026); with `StrictHostKeyChecking=no` OpenSSH then
  DISABLES password auth (the strih-snv log reader's rule). `win_ssh_run` and the bundle-state
  self-heal's ssh do not carry it: they break on a re-imaged box until the stale key is removed
  (`ssh-keygen -R 10.77.9.204`, a dev1 step).
- **Never source `win-ssh-exec.sh` from the lib** (its top-level `set -euo pipefail` would leak): the
  callers source it; a Windows box with no `win_ssh_run` defined is a named rc 127.
- **A `timeout`-bounded call re-sources BOTH libs** in its `bash -c` (timeout cannot exec a function):
  `bash -c '. "$1"; . "$2"; stream_box_exec "$3" "$4" "$5" "$6" "$7"' _ "$HERE/lib/win-ssh-exec.sh" "$HERE/lib/stream-box.sh" ...`.

## The consumers (ported in issue 1462)

- rig-mode TEST `#901 item 2` (`verify_mbc_dante_transport`): on a Linux stream box it passes
  `--vban-stream S --vban-port P` to `obs_phase2.py mbc-input-check`, which then grades the obs-vban
  RECEIVE source (pure `_mbc_vban_transport_problem`: mute first, then the kind
  `program_audio_vban.VBAN_SOURCE_KIND`, then `settings_differences` for the stream + port, so an
  absent key is the source's default). The Receive Buffer is not graded there (verify item 9 NOTEs it).
  Without the two flags the Dante device check is byte-identical.
- rig-mode TEST `#901 item 1` + recording-e2e `[4b2/8]`: the volumedetect + delete through
  `stream_box_exec`. recording-e2e `[4b3/8]`: the WAV extract + scp pull-back + delete; the box-side WAV
  is `AUDIO_DECODABILITY_WAV_LX` (default `/tmp/camera-box-mbc-decodability.wav`, removed after).
- recording-e2e `[0/8]` session-visibility: a Linux stream box gets `stream_box_visibility_gate_linux`;
  the Windows leg stays byte-identical inside the `else` (pinned by `harness_strih_platform_1351.rs`).
  Mind the 3200-byte windows of `harness_obs_session_visibility_977.rs` from the gate banner: the
  Linux branch is two lines so the Windows recovery text still sits inside them.
- recording-e2e `[0/8]` dantesync version gate: a Linux stream box moves from `--win` to `--linux`
  (`DV_WIN_NODES` loses its `stream=` word), never in both arms.
- The bundle-state self-heal (`bundle_state_selfheal_class`): a non-strih host takes its OWN fleet
  class, so the Linux stream box gets the `systemctl --user` restart; `STRIH_PLATFORM` never applies
  to it.

## Still Windows-only (follow-up candidates, not ported)

- The version-integrity gate's Windows-only facets for `--win-state stream=` (it has `--strih-linux`
  for the strih only): the `[0/8]` gate reads UNKNOWN for stream-snv until a per-box Linux flag exists.
- The stream recording decode at `[8/8b]` (`recording-verdict-on-stream.sh`, `win_ssh_upload` of the
  marker CSV + switch schedule, the `C:\camera-box` paths, the `win-stream-snv` cleanup plan lines, the
  missing-slot-pixels `.cmd` branch): needs a Linux sibling like `recording-verdict-on-strih-snv.sh`.
- The opt-in restart / zero-loss modes' stream decode plans; the legacy `VERDICT_ON_STREAM=0`
  `recording-fetch-windows.sh` path; the obs-fleet `win-baseline` facet still lists stream-snv
  (report-only rows that read unread).

## Tests (Tier-0, no rig)

`tests/python/test_stream_box_lib_1462.py` (the lib, the builders RUN on a fake box),
`test_stream_box_consumers_1462.py` (rig-mode sourced; recording-e2e steps cut between count-1 anchors
and run under `set -euo pipefail`; Linux = the default fleet, Windows = an `OBS_FLEET` override that
keeps the PowerShell text), `test_mbc_vban_transport_1462.py` (the pure decision + the handler + the
CLI). Fakes: `tests/python/stream_box_fakes_1462.py` (a fake `sshpass` that RUNS the bash text on a
stub PATH and decodes the PowerShell it would send).
