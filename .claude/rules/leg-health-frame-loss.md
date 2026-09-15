---
paths:
  - "scripts/lib/leg-health-guard.sh"
  - "tests/harness_leg_health_guard_1133.rs"
---

# Leg-health preflight — the capture-health signal is FRAME LOSS, never DEQUEUE STALL (#1133)

The `[0/8]` per-box leg-health preflight (`scripts/lib/leg-health-guard.sh`, wired in
`scripts/recording-e2e.sh`) aborts a run when a capture leg is sick. Its HARD signals are:
sustained **capture frame loss**, emit-gate **SKIP** aggregates, and kernel **EPROTO**. Its
REPORT-ONLY diagnostics (surfaced, never abort): the **DEQUEUE STALL** count and the cap-1s
over-rate band.

## NEVER re-gate on the DEQUEUE STALL count — it is ANTI-correlated with real health (#1133 / issue 1198)

`#707 V4L2 capture DEQUEUE STALL` times the BLOCKING `VIDIOC_DQBUF`, which WAITS for the next
frame. So its duration measures whether the capture thread arrives at the dequeue EARLY or LATE,
NOT device health:
- a well-protected thread finishes its work in a fraction of a ms, arrives early, and legitimately
  waits ~a whole frame interval (~16.7 ms) — ~8 ms of jitter tips it past the warn line → MORE stalls;
- a poorly-scheduled/lossy thread arrives late, a buffer is already queued, the call returns almost
  instantly → reports ZERO stalls while frames are genuinely being lost.

Fair paired test (issue 1198, same binary, same load, only the capture core varied): the arm
losing **4.5× FEWER** frames (1.30% vs 5.93%) reported **MORE** stalls; the worst arm reported
**zero**. Raw `v4l2-ctl` on the same card/cable/port under the same 8-ssh load held 59.97–60.00 fps
with no loss — so the old "capture-device/USB fault, replace cable/port/grabber" wording was a
**misattribution**. If you find yourself wanting to gate on stall COUNT again: don't. Gate on loss.

## The frame-loss metric — sent vs captured from the `Streaming:` line (field mapping is easy to get backwards)

The appliance already logs every ~5s (src/main.rs, the genlock-box form):
```
Streaming: <e> fps emitted / <c> fps captured (<N> sent, <M> captured, <K> capture-dropped, <C> corrupted)
```
- `<N> sent` = `emit_count` = the genlock **SEND cadence** (what the output demanded this window).
- `<M> captured` = `frame_count` = frames **actually captured from the device** this window.
- When capture drops frames, the emit gate REPEATS the last frame to fill, so **`N >= M`** normally,
  and **per-window LOST = max(0, N - M)** is exactly the frames the device failed to deliver.
- **Over-rate (`M > N`, a ShadowCast capturing 61-63fps) contributes 0 loss** — it is benign,
  absorbed by the decimation (issue #909). Always clamp negative to 0; never treat over-rate as loss.
- The non-genlock `Streaming: <c> fps (<f> frames, ...)` form has **no ` sent, `** and is skipped by
  `leg_health_streaming_grep_pattern` — only the parenthetical `<N> sent, <M> captured` form is parsed.

## Calibration + the sustain rule (bands do NOT overlap)

Supervisor-measured live fleet 2026-08-20 (12 windows/box): healthy cam1 0.277% / cam2 0.222% /
cam3 0.028% / cam4 0.000%; historically DEFECTIVE cam1 2.53% and 7.60%. The gate FAILs only when
ALL THREE hold (single-condition would false-fail a healthy box or fail on one bad window):
(a) `>= 5` windows (else insufficient data → PASS — a just-restarted box is never judged), (b)
aggregate loss `>= 1.25%` (4.5× above worst healthy, 2.0× below least-bad defective), (c) `>= 3`
individually-elevated (`>=1.25%`) windows — the SUSTAIN guard so a single catastrophic window
never fails a run. Any threshold/sustain change is a DATA-FIRST step: re-mine live-fleet windows,
keep the healthy band passing AND both historical defective reads (2.53% / 7.60%) failing.

## What this metric does NOT catch (by design — other signals cover it)

Frame loss is a CAPTURE-loss measure for a box that IS producing output. A **dead leg** (0 sent, 0
captured) clamps to 0 loss → PASS here, but is caught by emit-SKIP (hard), EPROTO (hard), the #656
capture-rate DEFECTIVE WARN, and emit_freeze. Do not try to make the frame-loss gate a liveness
check — keep those signals separate.

## The EPROTO term counts only STEADY-STATE -71 — restart-adjacent artifacts are excluded (#1133 reopen)

A kernel `uvcvideo … Non-zero status (-71)` (EPROTO) is NOT only a wire fault — it is ALSO emitted
once per UVC stream teardown/re-open, i.e. once per `camera-box.service` restart AND per
`camera-box-burn-*` transient-unit start/stop, at the SAME second as the restart. So the harness's
OWN restart churn manufactures -71 lines. Reopen evidence (15.9.2026, E2E run 34977348169 refused
on cam4): nine `camera-box` restarts in two hours of chained runs (the parity auto-align of
.628/.629/.630, the main deploy of .629, the `[2/8]` device free/restore, and each run's cleanup
restart) produced exactly ONE `-71` per restart at the same second — `over_current_count` 0 on
every port, zero `-71` in steady state, the camera-box journal itself carrying none. The runs were
chained (back-to-back), so the trailing-hour count crossed the ≥6/hr bar purely from restart churn;
the morning runs stayed under only because they were spaced further apart.

The gate reads BOTH journals with epoch timestamps in the one `read_all` ssh round-trip and counts
only STEADY-STATE -71:
- `leg_health_eproto_restart_adjacent_secs()` = **3 s** (covers the parity-align stop→start gap,
  e.g. `13:24:58 → 13:25:01`, while far tighter than the ~minutes between run restarts).
- `leg_health_eproto_ts_read_cmd` reads the -71 epochs (`journalctl -k -o short-unix … | grep -E
  'uvcvideo.*Non-zero status' | grep -oE '^[0-9]+'`).
- `leg_health_restart_ts_read_cmd` reads the `camera-box`/`camera-box-burn-*` lifecycle epochs. It
  is a FULL-journal read (NOT `-u 'camera-box*'` scoped): a transient burn unit is stopped +
  reset-failed after each run, so a `-u` glob may no longer resolve it, but its `systemd[1]`
  lifecycle line survives the full journal. Three chained greps — the `systemd[<pid>]:` process
  signature, then `camera-box`, then a lifecycle verb (`Started|Starting|Stopping|Stopped|
  Deactivated`) — match in either field order on the fleet's Ubuntu-noble systemd 255 (the unit
  NAME is in the Started/Stopped message there), while an APP log line (`camera-box[PID]: Started
  …`, not `systemd[…]:`) and a foreign unit's lifecycle line are both rejected.
- `leg_health_eproto_steady_count EPROTO_TS RESTART_TS` (pure, fixture-testable) echoes
  `"STEADY ADJACENT"`: a -71 within ±3 s of ANY restart epoch is ADJACENT (excluded); the rest are
  STEADY and feed the ≥6/hr bar via `leg_health_classify`. A -71 with NO restart data at all stays
  STEADY (a failed ssh read of the restart journal must never MANUFACTURE an exclusion that hides a
  real wire fault). The `[0/8]` `ok:` line reports the excluded count
  (`eproto=N steady (M restart-adjacent ignored)`).

A GENUINE wire fault — `-71` BETWEEN restarts (no restart within ±3 s) — is unaffected and still
refuses at the same ≥6/hr bar. The restart read window is widened by ±the adjacency so a -71 at the
1-hour window edge still sees a restart just outside it.

## Tier-0 verification (no cargo)

The lib is pure bash; `tests/harness_leg_health_guard_1133.rs` shells to `bash` to source it. Verify
RED→GREEN LOCALLY by sourcing the lib and calling the functions directly over fixtures/inline text
(exactly what the harness does), then `cargo fmt --all --check` to prove the `.rs` parses. Report-only
helpers (`leg_health_dequeue_stall_report`, `leg_health_cap1s_band_warn`) are called as BARE
statements under the caller's `set -euo pipefail` → they MUST return 0 on every input (empty read,
grep no-match) — see `.claude/rules/ci-testing-gotchas.md`'s report-only-under-set-e entry.

To exercise a REMOTE-READ BUILDER (`leg_health_eproto_ts_read_cmd` / `leg_health_restart_ts_read_cmd`,
which emit a `journalctl … | grep …` string) end-to-end WITHOUT a rig, override `journalctl` as a
bash FUNCTION that `cat`s a `-o short-unix`-shaped fixture, then `eval` the builder's emitted command:
`journalctl() { cat fixture.txt; }; CMD=$(leg_health_restart_ts_read_cmd 100 3700); eval "$CMD"`. The
function shadows the real binary in that shell, so the whole grep pipeline (epoch extraction + the
systemd-signature/camera-box/lifecycle-verb filter) runs against the fixture — the strongest local
proof that the reader keeps only the intended epochs (the #1133 harness's fake-journalctl tests do
exactly this). NOTE: a worktree-isolated worker's guard may refuse a `bash -c '…source lib…'` /
`eval` shape (`.claude/rules/ci-testing-gotchas.md`, #1265) — the sourced-lib + fake-journalctl
harness then runs at CI / for the supervisor; a worktree worker falls back to `python3 -c` grep-chain
simulations of the emitted command.
