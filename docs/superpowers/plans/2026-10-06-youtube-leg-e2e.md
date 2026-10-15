# YouTube leg + CG path in the release E2E — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every camera-box release E2E run proves, frame by frame and with continuous sound, that the YouTube VOD shows what stream OBS sent. It also proves that the CG/SongPlayer path (SongPlayer `SP-program` and cg OBS via "OBS manuál") delivers its picture and sound without stutter.

**Architecture:**
- Restreamer owns the YouTube session lifecycle behind `/api/v1/av-gate/session` on stream.lan.
- Camera-box owns one measurement tool, `scripts/youtube_leg_verdict.py` (pure functions + a thin CLI), a dev1 mirror of cam2's marker log, a session client, and the wiring into `recording-e2e.sh` through a sourced lib.
- The verdict folds into the run's verdict JSON, so the existing `#703` fail-closed guard and the Discord report see it.

**Tech Stack:**
- Python 3 (stdlib + numpy + OpenCV `cv2`, already used on dev1);
- ffmpeg, yt-dlp;
- the CI `probe-tools-linux-amd64` `recording-verdict --av-sync`;
- bash sourced libs;
- pytest for Tier-0 tests (no cargo compile).

**Spec:** `docs/superpowers/specs/2026-10-06-youtube-leg-e2e-design.md` (approved by the owner 6.10.2026, issue 1404 comment 6016272520; section E added by the owner's decision, comment 6016291989).

## Global Constraints

- PASS criteria, verbatim from the spec:
  - A/V VOD − recording within ±150 ms of the run's first window;
  - 0 downstream dup/skip;
  - first VOD frame ≤ 0.5 s after every publish;
  - audio: no lag jump > 1.5 ms, no low-correlation block (< 0.6) while the recording has signal, no silent VOD block (< −70 dBFS), no level drop > 10 dB;
  - a window under 90 % cadence-proven frames is UNKNOWN.
- Exit codes: 0 PASS, 1 FAIL, 2 UNKNOWN. UNKNOWN is red (fail closed) everywhere.
- Restreamer API: `POST /api/v1/av-gate/session {requester}` → `{session_id, broadcast_id}`; `GET /api/v1/av-gate/session/{id}` → `starting|ready|processing|done{vod_id}|failed{reason}`; `POST .../{id}/stop`. LAN only, existing API token. One session at a time, and the caller holds the rig lease.
- YouTube quota: ≤ ~10 sessions/day across both gates. The YouTube leg runs at most once per head SHA.
- Marker log: `http://dev1:8890/rig-qpsk-markers.csv`, plain CSV identical to cam2's `/run/rig-qpsk-markers.csv`. No other project ever uses the fleet ssh credentials.
- Never put `PRO` on the stream program or preview (issue 1380 guard, `obs_phase2._rpc`).
- Tier-0 on dev1: no local cargo compile. Tests are pytest or plain bash; Rust changes are verified by `cargo fmt --all --check` + CI.
- Commit messages: `feat(#1404): ...` / `test(#1404): ...`, RED before GREEN for behaviour, no other `#N` in messages.

## Review Focus

1. **A VOD still `processing` at the deadline, or a quota-exhausted session:** the run must go UNKNOWN (red), never PASS and never skip silently. Test in Task 3.
2. **A recording split by an OBS restart** (two files): tick joining must not invent a gap or a dup at the seam. Test in Task 1 with the session-3 two-part fixture.
3. **A VOD that starts AFTER the window start** (pre-LIVE content missing): the window start must clamp to the first frame present in both files and report it, not crash or FAIL. Test in Task 1. The session-3 A window hit exactly this, comment 6014624.
4. **Re-running the E2E on the same head SHA:** it must reuse the stored YouTube verdict and never open a second session. Test in Task 4.
5. **The colour-coded painter QR on the left half** (frames undecodable with a left-only decoder): the decoder must read both halves, and the cadence-proven % must stay ≥ 90 % on the session-3 part-1 fixture. Test in Task 1.


## Owner amendment (6.10.2026, issue 1404 comment 6016489928): nothing copyrighted on YouTube

Owner, verbatim: "nemozes nejaku len tak hudbu z songplayera pustat do youtube lebo to moze sposobit ban, ved nech to je normalne qr meracie video aj s zvukovym meracim generatorom".

Changes against the tasks below. They supersede any conflicting wording there.

1. **Task 5 content:** the SongPlayer test item is NOT a cached music item. It is a camera-box-generated measurement file, `scripts/gen_measurement_clip.py`, which writes `tests/fixtures/...` and the deliverable `measurement-clip-v1.mp4`:
   - 1080p30, 120 s;
   - picture: a per-frame QR `P911016.{frame}.{pts_ns}.{crc}` (new reserved run id, an origin like 911014, never a camera node), the same QR geometry as the cam2 painter, plus a frame counter;
   - sound: 48 kHz stereo, the QPSK marker (the same modulation and word format as `src/qpsk_marker.rs`, every 0.5 s, its index = frame index / 15) over a −30 dBFS 1 kHz tone bed;
   - the generator and the decoder share one parameter set;
   - the tone bed frequency is `scripts/program_audio.py` `MEASUREMENT_TONE_LINES_HZ` (import it): the program-audio guard removes exactly that line, so any other bed frequency makes every CG session read FOREIGN and stop (issue 1404 Task 2 review);
   - the cg OBS test scene `E2E test (cg)` plays the same file in a media source;
   - SongPlayer plays it as a local test item (songplayer issue 228, option 2).
   - The CG sound check decodes the QPSK marker in the stream recording and the VOD, which proves continuity and A/V with the same instrument as the camera chain. The block correlation against the known file is the second, exact check. No music FLAC is used anywhere.
2. **Task 4 safety guard (a new step):** before `youtube_leg_start` and every 10 s while the broadcast is live, read stream OBS's program audio level (the `InputVolumeMeters` peak of the inputs in the program tree, the `measurement_audio_meter_probe.py` method).
   - Content louder than the measurement band, i.e. the existing issue-1323 "POLLUTED" bar of max > −20 dBFS on `mbc`, means non-measurement audio (music, a rehearsal) is on the program.
   - Then: StopStream at once, `stop` the session, and the YouTube leg is UNKNOWN (red), with the reason "non-measurement audio on the program — broadcast stopped to protect the channel".
   - Test: a fake meter feed above the bar triggers the stop within one poll.
3. **Spec section E** is updated the same way: measurement content only, sound = QPSK + tone.

---

### Task 1: The measurement tool `scripts/youtube_leg_verdict.py`

**Files:**
- Create: `scripts/youtube_leg_verdict.py`, the pure functions + CLI. Port from `~/.claude/work-products/issue-1404/`:
  - `qrticks.py` (decode), extended to both halves as in `bothsides.py`;
  - `session2/dupskip.py`;
  - `audiocont.py`;
  - `avabs2.py` (the per-window A/V clip cut + `recording-verdict --av-sync`).
- Create: `tests/python/test_youtube_leg_verdict_1404.py`
- Create fixtures in `tests/fixtures/youtube_leg_1404/`, gzip-compressed and cut to the windows:
  - tick maps: baseline, session 2, session-3 parts a/b, VODs;
  - saved `*.avsync.out`;
  - 20 s 16 kHz mono FLAC clips cut from `~/.claude/work-products/issue-1404/audio/` for one window per session.

**Interfaces (Produces):**
- `load_ticks(path, pts_offset=0.0, idx_offset=0) -> list[tuple[int, float, int|None]]`
- `join_parts(parts: list[tuple[path, record_start_utc_s]]) -> (rows, t0)`: multi-part recordings joined by tick.
- `dupskip(rec_rows, vod_rows, t0, a, b) -> {"dup": int, "skip": int, "unjudged": int, "rec_frames": int, "vod_frames": int}`
- `continuity(rows) -> {"frames", "decodable", "cadence_proven", "events"}`
- `publish_gaps(rec_rows, vod_rows, t0, publishes_utc) -> list[{"utc", "first_vod_frame_utc", "gap_s"}]`
- `audio_window(rec_audio, vod_audio, rec_rows, vod_rows, t0, a, b) -> {"blocks", "lag_jumps", "low_corr", "silent", "level_drops", "corr_median", "lag_vs_video_ms"}`
- `av_window(rec_file, vod_file, rec_start_s, vod_start_s, markers_csv, probe_bin) -> {"rec_ms", "vod_ms", "delta_ms", "markers_rec", "markers_vod"}`
- `verdict(windows, publishes) -> {"overall": "PASS"|"FAIL"|"UNKNOWN", "reasons": [...]}`
- CLI as in the spec. It writes `<out>/youtube-leg-verdict.json` and exits 0/1/2.

- [ ] **Step 1: Write the failing tests (real-session fixtures)**

```python
# tests/python/test_youtube_leg_verdict_1404.py
import importlib.util, pathlib
FIX = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "youtube_leg_1404"
spec = importlib.util.spec_from_file_location("ylv", pathlib.Path(__file__).resolve().parents[2] / "scripts" / "youtube_leg_verdict.py")
ylv = importlib.util.module_from_spec(spec); spec.loader.exec_module(ylv)

def test_session2_steady_windows_have_zero_downstream_dupskip():
    rec = ylv.load_ticks(FIX / "s2-rec-ticks.tsv.gz"); vod = ylv.load_ticks(FIX / "s2-vod-ticks.tsv.gz")
    t0 = 23*3600 + 22*60 + 22.405
    for a, b in [(23*3600+23*60, 23*3600+31*60+25), (23*3600+33*60+10, 23*3600+39*60+25)]:
        r = ylv.dupskip(rec, vod, t0, a, b)
        assert (r["dup"], r["skip"]) == (0, 0)

def test_baseline_republish_window_fails_on_dupskip():
    rec = ylv.load_ticks(FIX / "base-rec-ticks.tsv.gz"); vod = ylv.load_ticks(FIX / "base-vod-ticks.tsv.gz")
    t0 = 20*3600 + 22*60 + 43
    r = ylv.dupskip(rec, vod, t0, 20*3600+33*60+35, 20*3600+34*60+15)
    assert r["dup"] >= 40 and r["skip"] >= 40          # measured 46 / 47

def test_session3_two_part_join_has_no_seam_artefact():
    rows, t0 = ylv.join_parts([(FIX / "s3-rec_a-ticks.tsv.gz", 1*3600+40*60+15.901), (FIX / "s3-rec_b-ticks.tsv.gz", 1*3600+51*60+53.503)])
    c = ylv.continuity([r for r in rows if r[1] + 0 >= 0])
    assert c["events"] <= 20                           # only the rig-side events listed in comment 6011155324

def test_publish_gap_session3_restart_is_under_half_a_second():
    rows, t0 = ylv.join_parts([(FIX / "s3-rec_a-ticks.tsv.gz", 6015.901), (FIX / "s3-rec_b-ticks.tsv.gz", 6713.503)])
    vod = ylv.load_ticks(FIX / "s3-vod-ticks.tsv.gz")
    g = ylv.publish_gaps(rows, vod, t0, [1*3600+52*60+8.12])
    assert g[0]["gap_s"] <= 0.5                         # measured 0.08 s

def test_window_starting_before_the_vod_clamps_and_reports():
    rows, t0 = ylv.join_parts([(FIX / "s3-rec_a-ticks.tsv.gz", 6015.901)])
    vod = ylv.load_ticks(FIX / "s3-vod-ticks.tsv.gz")
    r = ylv.dupskip(rows, vod, t0, 1*3600+40*60+40, 1*3600+49*60+28)
    assert r["clamped_start_utc"] is not None and (r["dup"], r["skip"]) == (0, 0)

def test_audio_clean_window_passes_and_a_cut_block_is_a_lag_jump(tmp_path):
    rec = ylv.load_audio(FIX / "s2-R-rec.flac"); vod = ylv.load_audio(FIX / "s2-R-vod.flac")
    ok = ylv.audio_blocks(rec, vod)
    assert ok["lag_jumps"] == 0 and ok["silent"] == 0 and ok["level_drops"] == 0
    cut = ylv.drop_samples(vod, at_s=10.0, ms=23)       # one lost AAC frame
    bad = ylv.audio_blocks(rec, cut)
    assert bad["lag_jumps"] >= 1

def test_verdict_unknown_when_coverage_below_90_percent():
    w = {"coverage": {"rec_cadence_pct": 83.5, "vod_cadence_pct": 96.3}, "av": {"delta_ms": 0}, "dupskip": {"dup": 0, "skip": 0},
         "audio": {"lag_jumps": 0, "low_corr": 0, "silent": 0, "level_drops": 0}}
    assert ylv.verdict([w], [])["overall"] == "UNKNOWN"
```

- [ ] **Step 2: Run the tests to verify they fail.** Run `python3 -m pytest -q tests/python/test_youtube_leg_verdict_1404.py`. Expected: FAIL (module missing).

- [ ] **Step 3: Implement.** Port the four session scripts into the module as the listed pure functions. The ported logic is already validated against the sessions (comments 6006986090, 6008636005, 6014624254). Then:
  - add the both-halves decode (`bothsides.py`: decode the left and right half separately and keep the even-tick value; the right QR carries tick+1);
  - add `clamped_start_utc` to `dupskip` when the VOD lacks the window start;
  - add `load_audio`, `audio_blocks` (the 0.25 s xcorr core of `audiocont.py` with the 4 s ±2.5 s initial lock) and `drop_samples` (a test helper);
  - add `verdict()` with the spec criteria.

- [ ] **Step 4: Run the tests.** Expected: PASS.

- [ ] **Step 5: Commit.** `git add scripts/youtube_leg_verdict.py tests/python/test_youtube_leg_verdict_1404.py tests/fixtures/youtube_leg_1404 && git commit -m "feat(#1404): ..."`

### Task 2: The marker-log mirror on dev1

**Files:**
- Create: `scripts/rig-marker-mirror.sh`. One pass: scp cam2 `/run/rig-qpsk-markers.csv` (via `camera_resolve CAM2`) into `$RIG_LEASE_SERVE_DIR/rig-qpsk-markers.csv` through a temp + atomic rename, and fail loud on error.
- **As built (Task 2 lane, review rounds 1-2, issue 1404 comment 6024708653):** one cam2 login writes 11 lines into cam2's persistent stick journal, so the mirror is a long-running `--user` service holding ONE ssh connection (`stat` size, then `tail -c +1 -F --pid=$PPID`), not a 10 s scp timer; the serve dir is `$XDG_RUNTIME_DIR/rig-lease-serve` (tmpfs, 0700). The program-audio sampler, guard and both routes are as specified, plus a FOREIGN latch (`--latch-s`, default 30 s). Details: `.claude/rules/program-audio-guard.md`.
- Create: `systemd/rig-marker-mirror.service` + `systemd/rig-marker-mirror.timer`, every 10 s, `--user`, shipped DISABLED like the other dev1 units.
- Modify: `scripts/rig-lease-server.py`: route `GET/HEAD /rig-qpsk-markers.csv` to the mirrored file, `text/csv`. Return 404 while it is absent, and add an `X-Mirror-Age-S` header.
- Test: `tests/python/test_rig_marker_mirror_1404.py`

**Interfaces:**
- Produces: `http://dev1:8890/rig-qpsk-markers.csv`.
- The server's existing `/rig-lease.json` and `/healthz` behaviour stays byte-identical.

- [ ] **Step 1: Failing tests.**
  - The server started on a temp port with a temp serve dir: `/rig-qpsk-markers.csv` → 404 when absent, 200 with the exact bytes and `X-Mirror-Age-S` when present, and `/rig-lease.json` unchanged.
  - The mirror script with a fake `scp` on PATH writing a fixture: an atomic rename happens and a failure exits non-zero.
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run → PASS, plus `bash -n` and `shellcheck -S warning` on the script.
- [ ] **Step 5:** Commit. Install on dev1 is a supervisor step: enable the timer, then `curl -sI http://dev1:8890/rig-qpsk-markers.csv` → 200.

### Task 3: The restreamer session client `scripts/av_gate_session.py`

**Files:**
- Create: `scripts/av_gate_session.py` with subcommands `start`, `wait-ready`, `stop`, `wait-done`.
  - `--base http://stream.lan:<port>`; the API token comes from an EnvironmentFile on dev1, never in the repo.
  - JSON on stdout, exit 0 OK / 2 UNKNOWN.
- Test: `tests/python/test_av_gate_session_1404.py`, against a fake `http.server` that scripts the state sequence.

**Interfaces:**
- `start(requester) -> {session_id, broadcast_id}`
- `wait_ready(id, deadline_s=360) -> "ready"`; raises `Unknown(reason)` on `failed` or timeout.
- `stop(id)`
- `wait_done(id, deadline_s=1500) -> vod_id`; raises `Unknown(reason)`.

- [ ] **Step 1: Failing tests.**
  - The happy path: `starting` → `ready`, then `processing` → `done{vod_id}`.
  - `failed{reason}` → exit 2 with the reason.
  - A deadline that passes in `processing` → exit 2 `vod not processed in 1500 s` (Review Focus 1).
  - HTTP 409 (session busy) on `start` → exit 2 `restreamer session busy`.
  - The token is sent as a header and never printed.
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement, stdlib `urllib` only.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5:** Commit.

### Task 4: Wiring into `recording-e2e.sh` + the verdict fold

**Files:**
- Create: `scripts/lib/youtube-leg.sh`, sourced. Functions:
  - `youtube_leg_start`: `av_gate_session.py start` + `wait-ready`, then StartStream on stream OBS via `obs_phase2.py`, recording the publish time.
  - `youtube_leg_stop`: StopStream + `stop`.
  - `youtube_leg_measure`: `wait-done`; pull the stream recording to dev1 the way `[8/8]` already locates it; fetch the markers from the dev1 mirror; run `youtube_leg_verdict.py` with the run's windows (the `[6/8]` sweep's window stamps) and the publish times; store the verdict under `~/.camera-box/youtube-leg/<head-sha>.json`.
  - `youtube_leg_cached <sha>`: reuse the stored verdict (Review Focus 4).
- Create: `scripts/fold_youtube_leg.py`. It adds a `youtube_leg` block to `verdict-<RUN_ID>.json` and sets `overall_pass = overall_pass and youtube == "PASS"`.
- Modify: `scripts/recording-e2e.sh`. Add only CALLS to the lib functions, placed after the existing anchored lines (the issue-675 prevention pattern; never edit an anchored line):
  - `youtube_leg_start` before `[5/8]`;
  - `youtube_leg_stop` right after the stream StopRecord in the main path;
  - `youtube_leg_measure` + the fold before the `#703` guard reads the JSON.
  - `cleanup()` calls `youtube_leg_stop` (idempotent) so an abort never leaves a live broadcast.
- Modify: the Discord per-run report (`compose_summary` / `compose_report`, see `.claude/rules/e2e-discord-report.md`): one YouTube line from the folded block.
- Test: `tests/python/test_youtube_leg_wiring_1404.py` (the lib with fake `av_gate_session.py` / `obs_phase2.py` / `youtube_leg_verdict.py` on PATH, the fold, the SHA cache), plus a static-anchor test that the three call sites exist and are unique.

- [ ] **Step 1: Failing tests.**
  - The fold turns `overall_pass` true + YouTube FAIL into false, and true + PASS stays true.
  - UNKNOWN → false.
  - A second run on the same SHA calls no `start`.
  - `cleanup` after a failed `wait-ready` still calls `stop`.
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run → PASS. Then run the full anchor sweep for `recording-e2e.sh` (the python occurrence-count sweep from CLAUDE.md, old vs new) and `bash -n`.
- [ ] **Step 5:** Commit. The live acceptance run on the rig is a supervisor step, once restreamer's API is deployed.

### Task 5: The CG / SongPlayer segments (spec section E)

Depends on SongPlayer songplayer issue 228 being deployed:
- the test playlist + `GET /api/v1/program` `{playlist_id, video_id, title, started_at_utc_ns, position_ms}` + the FLAC URL;
- `POST /api/v1/program/burn {"on"}` with `burn_on`;
- the facade cut via `ws://resolume.lan:4456`, `SetCurrentPreviewScene` + `TriggerStudioModeTransition`.

**Files:**
- Modify: `scripts/lib/cg-chain-songplayer.sh` and `scripts/cg_chain_scene.py`, re-targeted to today's topology:
  - no cg OBS scene mirror;
  - the SongPlayer segment = strih program on `CG-obs`;
  - the OBS manuál segment = the facade cut to the cg OBS scene `E2E test (cg)`, carrying burn 911015 and its test audio.
- Modify: `scripts/latency-pins-baseline.json`: drop the resolume `sp-*_video` sentinel (issue 1302 comment 6004895923).
- Modify: `scripts/lib/cg-chain-verify.sh` + `scripts/cg-chain-verify.sh`: the hop source becomes `CG-obs` on strih-lx / SP-program.
- Modify: `src/cg_chain_gate.rs`: `gates_overall_pass()` → true (BLOCKING) once the segments are wired.
- Create in `scripts/youtube_leg_verdict.py`: `audio_vs_reference(program_audio, reference_flac, start_s)`, the same block xcorr against the source FLAC; and `cg_av(burn_frames, audio_pos)`.
- Tests: extend `tests/python/test_cg_chain_scene_1302.py` and `tests/python/test_cg_chain_measure_1302.py` for the new topology. Add a reference-audio test (a clean clip passes, a 23 ms cut fails). The Rust seam flip is verified by fmt + CI.

- [ ] **Step 1: Failing tests** for the re-targeted scene plan:
  - never a cg OBS mirror;
  - the facade cut sequence with read-back;
  - the `PRO` guard;
  - the reference-audio checks.
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5:** Commit. A live run on the rig with SongPlayer and cg OBS is a supervisor step.
