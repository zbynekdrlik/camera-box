---
paths:
  - "scripts/av_gate_session.py"
  - "scripts/youtube_leg_e2e.py"
  - "scripts/youtube_leg_obs.py"
  - "scripts/e2e_report_youtube.py"
  - "scripts/lib/youtube-leg.sh"
  - "tests/python/test_av_gate_session_1404.py"
  - "tests/python/test_youtube_leg_e2e_1404.py"
  - "tests/python/test_youtube_leg_obs_1404.py"
  - "tests/python/test_youtube_leg_wiring_1404.py"
  - "tests/python/test_e2e_report_youtube_1404.py"
  - "tests/python/youtube_leg_e2e_fakes_1404.py"
---

# The full-path E2E's BLOCKING YouTube stage (issue 1404, main design 6383793524)

Owner 24.10.2026: "aby test bol end to end a nie len cast". The acceptance point (ROZHODNUTÉ 5.10)
is YouTube's CI test stream "e2e rtmp", read back as a viewer. Every verdict-executing full-path E2E
(`E2E_EXECUTE_VERDICT=1`, the release `pull_request` run) streams its measurement window to YouTube
and judges the VOD with the issue-1404 tool (`.claude/rules/youtube-leg-verdict.md`). There is no
switch to skip it in that mode. A refused delivery, a guard refusal, no VOD or a tool UNKNOWN is RED.

## The pieces

| File | Job |
|---|---|
| `scripts/av_gate_session.py` | restreamer's session API client: `start` / `status` / `wait-ready` / `stop` / `wait-done`, exit 0 OK / 2 UNKNOWN, one JSON line |
| `scripts/youtube_leg_obs.py` | StartStream / StopStream / record start on the stream OBS through `obs_phase2._conn` / `_rpc` |
| `scripts/youtube_leg_e2e.py` | the orchestration: `start`, `watch`, `stop`, `measure`; the state in `$OUTDIR/youtube-leg/state.json` |
| `scripts/lib/youtube-leg.sh` | the call surface for recording-e2e.sh (issue-675 pattern): `youtube_leg_start` / `_stop` / `_cleanup_stop` / `_gate` |
| `scripts/e2e_report_youtube.py` | the `youtube_leg` block in the Discord report + the per-point table (job summary + PR comment) |

## restreamer's interface (read-only contract; restreamer owns it)

- `/api/v1/av-gate/session` (restreamer `crates/rs-api/src/av_gate*.rs`, its rule
  `.claude/rules/youtube-av-measurement.md`): POST `{requester, title}` -> 201 `{session_id,
  broadcast_id}`; GET -> `starting` -> `ready` -> (after stop) `processing` -> `done {vod_id}` /
  `failed {reason}`; POST `/{id}/stop` -> 202 / 200. 409 busy / cleanup_pending, 429 quota, 502 a
  failed start (already torn down), 503 not_provisioned / starting_up.
- **`ready` needs OUR media:** it waits for the delivery VPS, then the YouTube stream `active`, then
  the live transition. So the order is guard -> POST -> StartStream -> wait `ready`
  (`YOUTUBE_LEG_READY_DEADLINE_S`, 420 s) -> `[5/8]` StartRecord. A refused POST never starts the stream.
- **The token never leaves the stream box.** The API is LAN-only + `Authorization: Bearer <content of
  av_gate.api_token_file>`. The client runs every request ON stream-snv over ssh: curl reads its
  header config from stdin (`curl -K -`), filled from the token file in place
  (`~/.config/restreamer/av-gate/api-token`, restreamer's Linux default; `--token-file` / `--api`
  override). The ssh password goes as `SSHPASS` (`sshpass -e`, env `AV_GATE_SSH_PASSWORD`), never argv.
- **State 24.10.2026:** Restreamer runs as `newlevel` on stream-snv (:8910 API, :1234 inpoint), but
  `~/.config/restreamer/av-gate/` does NOT exist there (the token + manage-scope oauth lived on the old
  Windows box), so the API answers 503 not_provisioned and the stage FAILS by name until restreamer
  provisions it. A missing token file reads "restreamer has not provisioned the YouTube A/V-gate
  session API there".
- restreamer's stop drains its event cache (~2.5 min) before completing the broadcast, then YouTube
  processes: `wait-done` (`YOUTUBE_LEG_DONE_DEADLINE_S`, 1800 s) runs after the merge, so most of that
  wait overlaps the on-box decodes. `vod_id` = the broadcast id.
- **Done = YouTube `uploaded` (restreamer 0.29.57, agreed 24.10.2026).** YouTube keeps a live archive at
  `processingStatus processing` for 30+ min while it is already fetchable, so restreamer reports done at
  lifeCycle `complete` + `uploaded`/`processed`/`succeeded`. The 1080p + audio check stays here:
  `fetch_vod` retries yt-dlp every `YTDLP_RETRY_S` (30 s) until `YTDLP_READY_S` (600 s, inside the
  1500 s tool timeout), then fails (UNKNOWN).
- Quota: one session of restreamer's rolling-24 h budget (~10 sessions/day across both gates) per run.
  There is no per-SHA cache (the spec's Review Focus 4): the stage runs on every run, and a cached FAIL
  could never pass on a rerun.

## Where it sits in recording-e2e.sh

- `youtube_leg_start` right before the `[5/8]` BEFORE-window snapshot (after `[4j/8settle]`). It runs
  the shared rig-busy guard first (`stray_session_check_assert`, StartStream is a rig mutation), then
  starts; a failed start stops whatever began and the run goes on (the gate ends RED with the reason).
  A live stage spawns the watch (background, `--parent $$`).
- The watch (every `YOUTUBE_LEG_WATCH_POLL_S`, 10 s): the program-audio channel guard
  (`program_audio_guard.py`; anything but exit 0 stops the stream + the session AT ONCE, the stage is
  failed "to protect the channel"), the recording's start (`GetRecordStatus.outputDuration`, read on
  the first poll after StartRecord), and a dead harness (it stops everything itself).
- `youtube_leg_stop` right after the stream StopRecord in `[7/8]` (StopStream, then the session stop).
- `cleanup()`: `youtube_leg_cleanup_stop` right after the StopRecord-first block: the StopStream half
  FIRST, in the foreground, `timeout 10` (`stop --stream-only`; a cancelled job is SIGKILLed seconds
  after its SIGINT), then the session stop in the background (never delays the device restores;
  idempotent). restreamer's 45-min idle reaper is the session backstop.
- **A cancelled run's leftover (review round 1).** A live stage writes the dev1 marker
  `~/.camera-box/youtube-leg-live.json` (`YOUTUBE_LEG_MARKER`, `--marker`: session id, host, stage dir,
  publish time) right before StartStream; a stop that finished both halves clears it (only when it
  still names that session). `youtube_leg_e2e.py recover` (always exit 0, prints what it did) stops the
  stream ONLY when `GetStreamStatus.outputDuration` puts the stream's own start within 120 s of our
  publish (`youtube_leg_obs.py stream-status`; any other live stream is never touched), and the session
  with `--with-session` (needs the ssh password). `scripts/rig-busy-gate.sh` runs it (stream half
  only) before its busy loop when the marker exists, because a stream we left would make that loop
  refuse every later run; `start` runs it with the session half, then a 409 busy whose holder is the
  recovered session is waited out (`BUSY_WAIT_S` 300 s, `--busy-poll`) while restreamer tears it down.
  The marker carries a provisional publish time before StartStream, overwritten after it.
  Known limit: `outputDuration` counts output frames, so a leftover that streamed for hours with many
  lagged frames can read older than 120 s off our publish; `recover` then refuses (the safe
  direction: the rig stays busy, never a foreign stream stopped) and the stream needs a hand stop.
- The camera-box dead-man's first-fire margin (`CAMERA_BOX_DEADMAN_OVERHEAD_FLOOR_MIN` 30 /
  default 35 min) and the painter slack (1800 s) both carry the stage's pre-record wait.
- `youtube_leg_gate` in BOTH tails, right after the program-audio gate line (before the Discord report
  and `exit "$GATE"`): pulls the stream recording to dev1 (`stream_box_download` in `timeout bash -c`),
  `measure` (VOD wait, windows, the verdict tool with `--probe-bin $PROBE_BIN_DIR/recording-verdict`,
  the fold), then deletes the two video copies (tick maps + JSON stay); a failed download removes the
  partial file first.
- Every outside step runs through `youtube_leg_proc.run_bounded` (a timeout kills the whole process
  group: yt-dlp, ffmpeg, the decode pool), and `install_cleanup()` kills them on SIGTERM / SIGINT.
- **Deadlines:** wait-ready 420 s, wait-done `YOUTUBE_LEG_DONE_DEADLINE_S` 900 s (it starts after the
  merge, ~10 min after the stop), the tool `YOUTUBE_LEG_TOOL_TIMEOUT_S` 1500 s. The painter's
  pre-record slack (`PAINTER_PRE_RECORD_SLACK_SECS`) grew 1200 -> 1800 for the ready wait. A run that
  still hits the job timeout leaves no block: the per-point page reads the stage dir (`--stage-dir`)
  and shows that as RED ("no verdict was folded"), never as "not measured".
- **Where the measurement runs:** dev1, as the main's design and the dispatch decided (never the
  stream box). It pulls the stream recording + the VOD (~1-2 GB together) and runs the 4-worker
  OpenCV decode there; both videos are deleted after the verdict. Moving it to a box that is not dev1
  is a main-session decision (the owner's no-rig-data-on-dev1 rule).
- The 703 byte window (merge -> `exit "$GATE"`): 12373 of 13000 bytes after this stage. The next step
  added there must widen it.

## Windows

The switch schedule's `[{cambox, start_ns, end_ns}]`, each segment trimmed 3 s at the head (the cut
+ settle) and 2 s at the tail (the tool wants a window to end >= 1 s before the StopStream), named
`wNN-<CAM>`; a segment under 10 s after the trim is left out and named in the reasons, and turns a
PASS into UNKNOWN (part of the recording was never judged on YouTube). No schedule (no sweep): one
window from record start + 5 s to the recording's stop - 3 s (`stop --record-stopped` notes it). `--publish` = the StartStream time
(the opening publish is reported, not judged), `--unpublish` = the StopStream time.

## The block + the report

- `youtube_leg` in the verdict JSON: `verdict` PASS / FAIL / UNKNOWN, `pass`, `gates_overall_pass:
  true`, `reasons`, ids, publish / unpublish / record start, per-window digest, the tool's criteria
  status, the stage events. The merge's `overall_pass` is left alone (the program-audio block pattern);
  the E2E folds the stage into `$GATE`, and `_run_passed` reads the block. The block is also written to
  `$OUTDIR/youtube-leg/youtube-leg-block.json` (uploaded) even without a verdict JSON. A crash anywhere
  in `measure` folds an UNKNOWN block naming it (never a missing block that reads like a pass).
- A red block never shows a green cell in the per-point table (a PASS criterion inside an UNKNOWN
  block reads ❔).
- PASS summary line: `(celá cesta kamera → YouTube)` only when the block passed; else `(kamera → stream
  nahrávka; YouTube ...)`. A red block = a `❌` run with one `YouTube (koncový bod)` line.
- The workflow step "Per-point E2E report" (`always()`, pull_request) writes
  `e2e_report_youtube.py --verdict ... --out` to `$GITHUB_STEP_SUMMARY` and `gh pr comment`
  (`pull-requests: write`). A missing verdict still posts "nothing verified".
- Job timeout 105 min (75 + the stage), the lease hold ceiling `RIG_LEASE_MAX_HOLD_SECS` 6300 with it
  (pinned by `test_the_default_hold_ceiling_matches_the_full_path_job_timeout`).

## Not in this slice (handed to the supervisor as follow-up candidates)

- A sample-level audio continuity check of the RECORDED stream audio (QPSK index chain + tone bed);
  the stream row's audio cell says "prijímač (počítadlá); vzorková kontrola nahrávky zatiaľ nie".
- The monitor cross-check rows (the stream av-sync dock LOCKED offset, dev2 SyncNet vs the gate).

## Tests (Tier-0)

`test_av_gate_session_1404.py` (a fake `sshpass` RUNS the remote bash with real curl against a fake
API on 127.0.0.1: header delivered, token never in argv/output, every refusal), `test_youtube_leg_e2e_1404.py`
(fakes for the four outside steps, `youtube_leg_e2e_fakes_1404.py`), `test_youtube_leg_obs_1404.py`
(an injected rpc), `test_e2e_report_youtube_1404.py` (the real PASS fixture), `test_youtube_leg_wiring_1404.py`
(the call sites + the lib RUN under `set -euo pipefail`). A test token must be low-entropy words
(the secret scan blocks a hex/digit run).
