---
paths:
  - "scripts/av_gate_session.py"
  - "scripts/youtube_leg_e2e.py"
  - "scripts/youtube_leg_obs.py"
  - "scripts/youtube_leg_host.py"
  - "scripts/e2e_report_youtube.py"
  - "scripts/lib/youtube-leg.sh"
  - "tests/python/test_av_gate_session_1404.py"
  - "tests/python/test_youtube_leg_e2e_1404.py"
  - "tests/python/test_youtube_leg_host_1404.py"
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
| `scripts/youtube_leg_e2e.py` | the orchestration: `start`, `watch`, `stop`, `recover`, `prefetch`, `measure`, `purge`; the state in `$OUTDIR/youtube-leg/state.json` |
| `scripts/youtube_leg_host.py` | the measurement host (dev2) over key-auth ssh: stage, pull from the stream box, early decode, the verdict tool, fetch-back, cleanup |
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
- `youtube_leg_stop` right after the stream StopRecord in `[7/8]` (StopStream, then the session stop),
  given `"${STREAM_HOST_PATH:-}"`: it then starts `prefetch` in the BACKGROUND (`prefetch.pid`,
  `prefetch.log`): the recording goes to dev2 and its tick map is decoded there during the merge and
  the VOD wait (design 6387919956 part b; next section).
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
  and `exit "$GATE"`): `measure --stream-path` (VOD wait, windows, the verdict tool ON dev2 with the
  probe binary + marker log shipped there, the fold). The recording never reaches dev1.
- `cleanup()` also runs a bounded background `purge` (stop a prefetch still running, delete dev2's
  stage dir); after a gate that measured it is a no-op.
- Every outside step runs through `youtube_leg_proc.run_bounded` (a timeout kills the whole process
  group: yt-dlp, ffmpeg, the decode pool), and `install_cleanup()` kills them on SIGTERM / SIGINT.
- **Deadlines:** wait-ready 420 s, wait-done `YOUTUBE_LEG_DONE_DEADLINE_S` 900 s (it starts after the
  merge, ~10 min after the stop), the tool `YOUTUBE_LEG_TOOL_TIMEOUT_S` 1500 s (on dev2, under the
  host's own `timeout`; the early decode has the same bound, `--decode-timeout`). The painter's
  pre-record slack (`PAINTER_PRE_RECORD_SLACK_SECS`) grew 1200 -> 1800 for the ready wait. A run that
  still hits the job timeout leaves no block: the per-point page reads the stage dir (`--stage-dir`)
  and shows that as RED ("no verdict was folded"), never as "not measured".
- **Where the measurement runs: dev2** (design 6387919956 part c; the owner's no-rig-data-on-dev1
  rule). dev1 orchestrates and folds the block; everything else is the next section.
- The 703 byte window (merge -> `exit "$GATE"`): 12373 of 13000 bytes after this stage. The next step
  added there must widen it.

## The measurement on dev2 (main design 6387919956, parts b + c, 24.10.2026)

- **Access:** plain key-auth ssh `newlevel@dev2` (`YOUTUBE_LEG_MEASURE_HOST`), the access the dev2
  avsync measurer already uses; no new credential. The stream box password the E2E holds
  (`AV_GATE_SSH_PASSWORD`) reaches dev2 on the ssh session's STDIN for `sshpass -e scp`, never in an
  argv. `scripts/youtube_leg_host.py` owns every remote step. The pull is Linux scp only: a
  Windows-class stream box (`stream_box_platform`, passed as `--stream-platform`) is refused by name.
- **Stage dir** `~/youtube-leg-1404/<dev1 run dir name>/` (`stage_key`, sanitized): `code/` (the
  tool's five modules, shipped from the E2E's own checkout every run, so dev2 needs no install of
  ours), `in/` (marker log + probe binary), `stream-recording.<ext>`, `verdict/` (the tool's out dir,
  the VOD lands here), `<step>.pid`. A stage dir older than a day (a killed run) is swept at the next
  run's start.
- **prefetch** (background, right after the stop): check -> ship the code -> pull the recording from
  the stream box straight to dev2 -> `--prime-ticks` (the early decode into the tool's cache). The
  outcome is `state.prefetch` = the block's `early` (`pulled`, `pull_s`, `decode_rc`, `decode_s`,
  `reason`, `unreachable`).
- **measure** waits for a running prefetch (bounded by the prefetch's own bounds, then stops it),
  pulls again ONCE when the early pull did not deliver (`early.retried_pull`), lets the verdict run
  decode the recording again when the early decode failed (`early.retried_decode`), ships the probe +
  marker log, runs the tool, fetches ONLY `youtube-leg-verdict.json`, `ticks-*.tsv` and their
  `.reads.json` (also after the tool ran over its bound), then deletes the stage dir (videos included,
  `measure_host` in the block). An unreachable dev2 (ssh exit 255) = UNKNOWN `the measurement host
  newlevel@dev2 is unreachable (...)`, never a measurement on dev1.
- **Bounds:** every long remote step runs as `timeout -k 30 <bound> nice -n 10 <cmd> &` with its pid
  in `<step>.pid` (timeout leads its own process group: the decode pool, yt-dlp's ffmpeg, the probe);
  the local ssh is `run_bounded` at the bound + 60 s, its stdin a closed pipe. A step's OWN exit 255
  (scp: the stream box unreachable from dev2) is returned as 254 with a stderr line, because 255 is
  ssh's own exit and means "dev2 unreachable". Exit 124 (or 137 at the bound) = "ran over"; 137
  inside the bound = "killed (exit 137)" (the OOM killer on a loaded dev2). `prefetch.pid` is removed
  by the prefetch itself, and a reader acts on it only when `/proc/<pid>/cmdline` is this stage's
  prefetch (a reused pid is never waited for or signalled). The cleanup kills every recorded group, then
  deletes the dir. 4 decode workers (`YOUTUBE_LEG_MEASURE_WORKERS`) at nice 10: dev2's other users
  (the SyncNet measurer, bakerion ffmpeg/nvenc jobs, node test runs) keep their share.
- **dev2 setup (one-time, supervisor):** key-auth dev1 -> dev2 (exists); `python3` with cv2 4.13.0
  (`opencv-python 4.13.0.92` in `~/.local`, exists) + numpy; ffmpeg, ffprobe, sshpass (exist);
  yt-dlp pinned to dev1's version: `pipx install "yt-dlp[default]==2026.8.19"` (installed
  24.10.2026; the remote PATH puts `~/.local/bin` first). dev2 has node 22 but no deno, so yt-dlp warns
  "No supported JavaScript runtime"; the 24.10 fetch still got 1080p + audio. If a fetch ever lacks
  formats: `~/.config/yt-dlp/config` with `--js-runtimes node`.

### Measured on dev2 (24.10.2026, the real files of E2E run 40110304642 attempt 3, dev2 load 13-36)

| Step | Time |
|---|---|
| pull 747 MB 4K recording stream-snv -> dev2 | 7.7 s |
| early decode of the recording (9316 frames, 4K) | 308 s (load ~15), 355 s (~17), 462 s (20-36) |
| VOD fetch (yt-dlp, 266 MB) | ~159 s |
| VOD decode (13470 frames, 1080p) | 131 s (load ~13), 139 s (~13), 254 s (~36) |
| A/V of ONE window, the recording clip (4K x264 cut + `--av-sync`) | 61.5 + 168.6 s |
| A/V of ONE window, the VOD clip (1080p) | 24.7 + 45.3 s |

The real run: job start 17:13:08Z, StopStream 17:32:59Z (+20 min), VOD done 17:44:55Z (+716 s).

**With the 1080p A/V read on the pool (ROZHODNUTE 6389130309), the final replay of that run on dev2**
(24.10.2026 20:57-21:22Z, dev2 load 11-34, the A/V pool sized 3):

| Step | Time |
|---|---|
| prefetch: pull 7.6 s + early decode 277.0 s | 286 s, inside the 716 s StopStream -> `done` wait |
| measure after `done`: VOD fetch | 133 s |
| VOD decode | 134 s |
| A/V of 10 windows, 1080p recording reads, pool 3 | ~925 s |
| fetch-back + stage cleanup | ~10 s |
| **the verdict tool, total** | **~1190 s of the 1500 s bound** (margin ~5 min) |

So the block is folded ~20 min after `done`: ~52 min into that job, against the 105 min job timeout.
The margin to the tool bound is load dependent. Each A/V slot ran ~277 s per window at load ~30, and
the A/V step is the largest term.

The verdict was a real one, not UNKNOWN:
- A/V PASS on all 10 windows: VOD - recording -14.6 to -15.4 ms, 49-51 markers each, every recording
  read "1080p of 2160p".
- dup/skip 0 in every window.
- FAIL on w10-CAM5, the last window before StopStream: "the VOD ends 8.7 s before the recording's
  last decoded frame of the window" and "the VOD audio ends 8.8 s before the window end". That is the
  existing fail-closed rule meeting a VOD that lacks the stream's last ~9 s. Whose tail that is
  (restreamer's drain, YouTube's archive cut, or the window placement near StopStream) is for the main.

### Replaying the stage on dev2 without the rig (how the numbers above were taken)

- The stream recordings stay on stream-snv (`/home/newlevel/<YYYY-MM-DD HH-MM-SS>.mp4`, local time =
  UTC+2 in summer) after a run, and the run's `$OUTDIR` on dev1 keeps the marker log, the schedule and
  `youtube-leg/state.json`. A replay = copy that state (drop `vod_id`), point
  `YOUTUBE_LEG_SESSION_CMD` at a stub that prints `{"ok":true,"state":"done","vod_id":"<id>"}`, then
  run `youtube_leg_e2e.py prefetch` and `measure` with the real `--stream-path`, markers, schedule and
  a CI `recording-verdict` (any `probe-tools-linux-amd64` copy). It reads one file from stream-snv,
  mutates nothing on the rig, and deletes its dev2 stage dir.
- Detach a long dev2 benchmark with `ssh -f newlevel@dev2 'setsid nohup ... > log 2>&1 < /dev/null &'`;
  without `-f` the local ssh stays attached until the remote job ends.
- Copying a scratch script or a run's marker log to dev2 from a lane with uncommitted edits is
  refused by the airuleset clean-tree deploy hook; a scratch copy that is not repo content takes
  `# airuleset:deploy-dirty-ok <reason>` on the scp line.
- Compare a decoder change on real files with `old vs new` tick maps row by row (changed / set /
  cleared halves, decoded ticks, 'x' rows), not only by a count.

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
(fakes for the outside steps, `youtube_leg_e2e_fakes_1404.py`: a fake ssh RUNS the remote bash under a
fake dev2 home), `test_youtube_leg_host_1404.py` (the prefetch, the measure on the host, the one
retry, an unreachable host, the bounds, purge, the stale sweep), `test_youtube_leg_obs_1404.py`
(an injected rpc), `test_e2e_report_youtube_1404.py` (the real PASS fixture), `test_youtube_leg_wiring_1404.py`
(the call sites + the lib RUN under `set -euo pipefail`). A test token must be low-entropy words
(the secret scan blocks a hex/digit run).
