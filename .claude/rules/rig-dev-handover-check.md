---
paths:
  - "scripts/rig-dev-handover-check.sh"
  - "scripts/rig_dev_handover_decision.py"
  - "scripts/lib/watchdog-roster.sh"
  - "tests/python/test_rig_dev_handover_1312.py"
  - "tests/python/test_rig_dev_handover_watchdogs_1319.py"
  - "tests/python/test_rig_dev_handover_exposure_1371.py"
---

# "development" handover check (#1312)

`scripts/rig-dev-handover-check.sh` is the ONE dev1 command the supervisor runs the moment the
owner writes „development" (any wording) after a production — it verifies the whole rig and prints
ONE Slovak checklist of what the owner forgot to switch back into development state, ending with a
`zabudol si: …` line the supervisor pastes verbatim to the owner. Owner directive 14.9.2026.

## Architecture — thin orchestrator + pure engine (do not merge them)

- **`scripts/rig-dev-handover-check.sh`** is I/O ONLY. Per item it runs an EXISTING read-only probe
  (never a new probe), bounded with `timeout` and drain-safe, and writes the probe's merged
  stdout+stderr to `$WORKDIR/<name>.out` and its exit code to `<name>.rc`. It **NEVER mutates the
  rig** — no `--fix` (a `--fix` mode is a followup, not this script).
- **`scripts/rig_dev_handover_decision.py`** is the PURE engine: it parses the captures
  (watchdog `verdict=`/`-> REACHABLE` lines, exit codes, the rig-mode bare token), maps every item
  to `OK` / `FORGOT-BY-OWNER` / `SUPERVISOR` / `FIXED-BY-ME` / `UNKNOWN` + a Slovak line, and decides
  the exit code (0 all-OK; 1 any FORGOT **or SUPERVISOR**; 2 UNKNOWN-only). `SUPERVISOR` (issue 1319)
  is a problem the SUPERVISOR must fix — the owner SEES it but is never blamed (`nezapnutý watchdog:
  …`, not `zabudol si: …`). **ALL logic + ALL tests live here** — Tier-0 #557:
  no cargo, `tests/python/test_rig_dev_handover_1312.py` feeds captured dry-run fixtures to the
  pure functions. Verify a change with `python3 -m pytest tests/python/test_rig_dev_handover_1312.py`;
  verify the bash with `bash -n` + `shellcheck -S warning` (the orchestrator has no cargo/live
  path in a worktree — it runs at CI / on the live rig).

## Items and the probe each one REUSES (never re-derive a probe)

| item | probe (read-only) | good → OK | forgot → FORGOT | else → UNKNOWN |
|---|---|---|---|---|
| mic | `measurement-audio-alert-watchdog.sh --dry-run` | `verdict=PRESENT` | `verdict=SILENT` (owner: odmutuj `mbc` v Abletone) | SKIP/UNKNOWN/cam2-down |
| mode | `lib/rig-mode-state.sh` (cam2 painter probe) | `TEST` | `EVENT` (supervisor: `rig-mode.sh test`) | cam2 down |
| painter | `optical-chain-alert-watchdog.sh --dry-run` | `verdict=healthy`/`log-only:*` | `verdict=alert:*` | `nothing to decide` |
| burns | `obs_burn_filter.py sweep-check` (strih+stream) | exit 1 (≥1 burn ON) | exit 0 (no burns) | exit 2 |
| mapping | `set-ndi-mapping.py --verify-only` (strih) | exit 0 | exit 1 | exit 2 |
| pins | `latency_pins_verify.py` (strih/stream/imag) | exit 0 | exit 1 | exit 2 |
| clock | `dantesync-clock-alert-watchdog.sh --dry-run` | `verdict=OK` | NO_CLOCK/NO_DANTESYNC/MGMT_DEAD | SKIP/UNKNOWN |
| obs | `obs-liveness-watchdog.sh --dry-run` | `verdict=HEALTHY` | FPS-ZERO/WEDGED-.../WS-DEAD/... | no verdict |
| net | `network-reach-alert-watchdog.sh --dry-run` | `-> REACHABLE` | `-> UNREACHABLE` | no line |
| audiolag | `audio-lag-alert-watchdog.sh --dry-run` | `verdict=HEALTHY` | LAGGING/STALE/DRIFTING | SKIP/UNKNOWN |
| genlock | `genlock-lock-alert-watchdog.sh --dry-run` | `verdict=HEALTHY` | DEGRADED/UNLOCKED | **UNKNOWN facet = UNKNOWN, NEVER forgot** |
| dantesync | `dantesync-version-gate.sh` | exit 0 | exit 20 (drift) | exit 11 |
| cambox | `camera-box-version-gate.sh` | exit 0 | exit 20 (drift) | exit 11 |
| avlatency | `measurement-chain-latency.sh` (marker emit vs `mbc` relative onset) | `verdict=ALIGNED` | `verdict=DRIFTED` (>90 ms vs baseline) | monotonic-emit / no-baseline / **ambiguous-cadence** (#1332) / chain-silent (all <−60 dB) / <3 onsets / cam2 down / stream OBS down |
| watchdogs | `systemctl --user` per timer in `lib/watchdog-roster.sh` (dev1-local, no ssh) | all enabled+active+fired <15 min → OK | disabled/inactive/never-run/stale → **SUPERVISOR** (`nezapnutý watchdog: …`, never `zabudol si`) | no unit file → UNKNOWN with names |
| exposure | `camera_test_settings.py snapshot-state` (dev1-local file, issue 1371) | `state=none` / `state=restored` → OK | `state=pending` / `state=invalid` → **SUPERVISOR** (the last EVENT switch did not put the owner's ISO/shutter back; summary `supervisor musí vyriešiť: …`) | no line → UNKNOWN |

## `avlatency` (#1312) — the mbc measurement-chain LATENCY, not just presence

The 14th item proves, between productions and read-only, that the mbc measurement-audio chain (cam2
painter QPSK marker → HDMI speaker → mic → mbc Ableton → Dante → stream OBS `mbc`) is still ALIGNED
with the video within the measurement-chain baseline-drift tolerance (`measurement_chain_latency.py`
`DEFAULT_TOLERANCE_MS`, ±90 ms vs baseline — a SEPARATE rig-health drift knob, NOT the per-camera A/V
gate `AV_OFFSET_GATE_TOLERANCE_MS`, which is ±30 ms since issue 1333) — the −140 ms A/V step of 14.9. is exactly the class it
catches. It reuses the whole verdict-kind item framework (like `mic`): the standalone
`scripts/measurement-chain-latency.sh` probe computes the token, the item maps `ALIGNED→OK`,
`DRIFTED→FORGOT`, everything else `→UNKNOWN`. All decision logic (RELATIVE rolling-floor onset
detection, the −60 dB `audio-presence-preflight.sh` bar as the SILENT-CHAIN guard only, pairing,
median, classify, the wall-clock guard) is the pure `scripts/measurement_chain_latency.py` kernel —
`python3 -m pytest tests/python/test_measurement_chain_latency_1312.py`.

- **NEVER the dock `av_offset_recent_med_ms`.** That signal is PIN-RELATIVE (it read +17 ms while the
  recording gate read −140 ms on 14.9.), so it can NOT be used as absolute. `avlatency` does an
  INDEPENDENT paired measurement: the cam2 marker log's emit times vs the stream `mbc` burst ONSETS
  timestamped off the OBS-WS `InputVolumeMeters` peak on dev1 — median per-marker latency vs baseline.
- **The MONOTONIC-EMIT trap (the standing prerequisite).** The ticket assumed `emit_ts_ns` is on the
  DanteSync wall clock. Historically the PERMANENT cam2 painter (`setup-device.sh`'s
  `cam2-painter.service`) emitted `start.elapsed()` MONOTONIC-since-painter-start ns — NOT comparable
  to the dev1 onset wall clock, and reset on every painter restart (which happens on every EVENT→TEST
  switch, i.e. exactly when this check runs). So the pure kernel GUARDS on the emit-ts SHAPE (a
  wall-clock ns is ~1.7e18; a monotonic elapsed value is orders smaller) and reads UNKNOWN
  (`neoverené`, reason `monotonic-emit`) when the emits are monotonic — NEVER a false forgot. This
  guard STAYS as a fail-safe.
- **The `--wall-clock` switch is SHIPPED (#1312).** Both the permanent `cam2-painter.service`
  ExecStart (`setup-device.sh`) and the transient `painter_launch_remote` (`rig-mode.sh`) now pass
  `--wall-clock`, so the painter stamps `emit_ts_ns` on `CLOCK_REALTIME` (the DanteSync wall clock).
  It is a SAFE no-op for the A/V verdict path (`av_sync_recording.rs:207` "no wall-clock alignment";
  `av_offset_candidates_with_fid` pairs by index→frame_id and ignores `emit_ts`) — the E2E burn
  painter already carried it. `avlatency` therefore becomes green-capable the moment the SUPERVISOR
  makes the change LIVE on cam2: `setup-device.sh` re-run, OR a remount-rw window that rewrites the
  unit ExecStart + `systemctl daemon-reload` + `systemctl restart cam2-painter.service`
  (provisioning/reboot-class). **Until the LIVE cam2 painter is restarted with `--wall-clock`, the
  running painter still emits monotonic ns and the item keeps reading `monotonic-emit` UNKNOWN — the
  code shipping is not the same as the box being re-provisioned.** After the switch is live, seed the
  baseline (below).
- **Baseline workflow.** The supervisor seeds `~/.camera-box/measurement-chain-latency-baseline.json`
  with `scripts/measurement-chain-latency.sh --baseline` right AFTER a green E2E (a known-aligned
  chain), and re-seeds it after any deliberate latency change. Until it is seeded the item reads
  UNKNOWN (`NO-BASELINE`), never a false forgot.
  - **The marker log must be FRESH (cam2-painter.service ACTIVE) or pairing reads 0 even with good
    onsets (#1312, live 2026-09-15).** A live read read `markers=79 onsets=9 paired=0`: the relative
    onset detection recovered 9 onsets off the ~−44 dB floor, but `cam2-painter.service` was `inactive`
    and `/run/rig-qpsk-markers.csv` was frozen ~385 s ago (a transient/leftover painter still sounded
    the marker), so the log's `emit_ts` were ~330 s behind the live onsets and none paired within the
    2 s window (cam2↔dev1 clock offset was 0.36 s — NOT the cause). Correct fail-safe (NO-BASELINE), but
    before `--baseline` confirm the SERVICE is active and the log's newest `emit_ts` is within seconds
    of `date +%s%N`, not just that onsets ≥ 3.
- **ALIAS-AWARE pairing with the baseline as a PRIOR (#1332, the 0.5 s-cadence regression).** Since
  issue 1318 the cam2 painter emits the marker every **0.5 s** (`--audio-marker-cadence-ticks 30`, to
  hold the measurement chain's gate/AGC open), so the ~1.13 s chain latency has FOUR candidate emits
  inside the 2 s pairing window. The old "latest prior emit" rule picked the alias `1132 − 2×500 ≈
  132 ms` → `classify` read DRIFTED vs the 1140.7 ms baseline → a FALSE `zabudol si` every handover.
  `pair_latencies` now pairs each onset to the candidate whose latency is CLOSEST to the persisted
  baseline (used as a **prior**); the emit cadence is derived from the log (`emit_cadence_ns`), and
  `pairing_is_ambiguous(cadence, window)` is `cadence < window`. Rules:
  - **With a baseline** → prior resolves the alias → real ~1132 ms → ALIGNED/DRIFTED normally. An
    unambiguous ≥-window cadence (the old 5 s) is unchanged (only one candidate, prior is a no-op).
  - **Without a baseline AND an ambiguous cadence** → the kernel does NOT pair and surfaces
    `ambiguous=True` → verdict UNKNOWN, reason `ambiguous-cadence` — **NEVER a false DRIFTED, and it
    beats NO-BASELINE** (in a 0.5 s cadence 132/632/1132 ms cannot be told apart from onset timing
    alone). This is the honest UNKNOWN, not a red.
  - **Seeding a baseline on an ambiguous cadence needs `--expected-ms <ms>`.**
    `measurement-chain-latency.sh --baseline` with an ambiguous cadence and no `--expected-ms` FAILS
    LOUD (exit 3, nothing persisted) so an aliased latency is never written; `--baseline --expected-ms
    1140` uses that value as the prior for pairing and persists the REAL measured latency (~1132).
- **TEST-premise (two-pass, like `mic`/`painter`).** The QPSK marker only sounds in TEST steady state,
  so on the first EVENT-mode pass `avlatency` reads UNKNOWN; it verifies once the rig is in TEST and
  the painter is back up. It samples the meter ~30 s then ssh-reads the marker log AFTER (so the
  just-emitted markers are present); the whole probe is bounded (`RDH_AVLATENCY_TIMEOUT`, default 100 s)
  so a down cam2 / stream box fails safe to UNKNOWN, never a hang.
- **Onset SIGNAL model — RELATIVE, not the absolute −60 dB bar (#1312, GOTCHA fixed 2026-09-15).**
  The earlier absolute onset detector armed only after a sample BELOW the −60 dB bar and fired on the
  next at/above it. That EXACTLY hit the predicted failure: a 45 s live read-only capture of the
  stream `mbc` peak shows the measurement mic's room/PA floor sits CONTINUOUSLY at ~−41…−47 dB (median
  −44.5, never below −60), so the detector never armed and returned `markers=145 onsets=0 paired=0
  verdict=NO-BASELINE` on a LIVE, working chain (the E2E A/V gate paired the same chain at −23 ms
  minutes earlier). **The mic floor is ~−45 dB on the rig; the absolute −60 dB bar is a SILENCE guard,
  never an onset bar.** `detect_onsets` now detects onsets RELATIVE to a rolling floor (median of the
  trailing `ROLLING_FLOOR_WINDOW`=20 samples ≈ 1 s at the ~50 ms WS cadence): a burst rises ≥
  `ONSET_DELTA_DB`=6 dB above that floor, re-arming with hysteresis at Δ/2. Δ=6 is calibrated from that
  capture — floor noise stays ≤ 3.8 dB above the rolling median (p99), the ~5 s cadence QPSK bursts
  rise +7…+25 dB, so 6 dB sits 2.2 dB above noise and catches every real burst (7 onsets on the 45 s
  fixture, clean ~5 s gaps). The −60 bar is KEPT only as `chain_is_silent`'s guard: every sample below
  it → 0 onsets, reason `chain-silent` (UNKNOWN, a #1310-class dead-audio problem, never a false
  drift). A flat but audible floor with no bursts still reads `too-few-onsets` (NOT `chain-silent`).
  Re-CALIBRATE Δ (`ONSET_DELTA_DB`) only from a fresh live capture if the venue floor/PA level moves
  the burst-vs-noise margin — never widen it so a marginal chain passes. The fixture
  `tests/python/fixtures/mbc_meter_live_2026-09-15.txt` is the reference capture.
- **The mixed-clock case cannot arise:** the permanent painter TRUNCATES the marker log on every start
  (`qpsk_emit.rs` #431 `File::create`), so a session's log is all-monotonic (today) or all-wall-clock
  (after the `--wall-clock` switch is live), never a mix — the pure kernel's `all()` wall-clock guard
  is exactly right. The supervisor's FIRST `--baseline` run must still confirm `onsets >= 3` (ideally
  6) over the window before trusting the written baseline.

## `watchdogs` (issue 1319) — the dev1 `--user` production-critical watchdog TIMERS

The 16th item catches the exact class the owner hit on 15.9.: `av-step-alert-watchdog.timer` +
`avsync-lineup-alert-watchdog.timer` had NEVER been installed on dev1, A/V-sync notifications
silently never fired, and NOTHING reported it. It reads (dev1-LOCAL, no ssh) `systemctl --user
is-enabled/is-active` + the timer's `LastTriggerUSec` age for every timer in the ONE roster
`scripts/lib/watchdog-roster.sh`, emits ONE raw per-timer line (`watchdog <t> scope=.. unit=..
enabled=.. active=.. age_s=..`), and the pure `classify_watchdogs` decides it.

- **The roster is the source of truth, NOT the test artefact.** `scripts/lib/watchdog-roster.sh`
  (`WATCHDOG_TIMERS`, entries `timer:scope`) is authoritative. The
  `_PRODUCTION_CRITICAL_TIME_BUCKETED` set in `tests/python/test_notify_dedup_key_sweep_1206.py`
  asserts a *notify-dedup* property and is a TEST artefact — keep the two conceptually aligned, but
  the roster is what the handover check reads. To add/remove a watchdog, edit the roster (and the
  `WATCHDOGS_OK` fixture in `test_rig_dev_handover_1312.py`'s full-fleet capture set).
- **A disabled/inactive/never-run/stale timer → SUPERVISOR, never FORGOT.** The owner cannot install
  a systemd unit; that is the supervisor's job. The Slovak line names the exact timers
  (`nezapnutý watchdog: <names>`) and the summary reads `supervisor musí zapnúť watchdogy: <names>`
  (exit 1) — the owner SEES the gap without being blamed. A timer with **no unit file** → UNKNOWN
  with the names (`chýba unit súbor pre watchdog: …`); nothing readable → UNKNOWN.
- **OK-recency bar = 15 min** (`WD_MAX_AGE_S`=900, the fleet's watchdog timers all run on ≤5-min
  cadences). An enabled+active timer that has not fired within it reads `off-stale` → SUPERVISOR.
- **imag-scoped timers are marked `:imag`** (`imag-obs-alert-watchdog.timer`) so
  `classify_watchdogs(imag_retired=True)` (env `RDH_IMAG_RETIRED`) drops them once imag is retired
  (issue 1316). imag-nb currently returns next year, so they are LIVE (counted) today.
- **NEVER mutates** — no `systemctl enable/disable/start`. The probe only reads.

## Gotchas / invariants

- **A DOWN box (cam2 is the standing one) must fail safe to UNKNOWN, never a hang, never a false
  OK.** Every probe is bounded with `timeout`; the mode/painter/mic items touch cam2 and read
  UNKNOWN when it is down. UNKNOWN is a non-zero exit (the check is not "clean"), but it is NEVER
  reported as FORGOT — only a probe that RAN and returned a bad verdict is FORGOT.
- **`scenes` + `Studio Mode` have no independent read-only probe** and are covered transitively by
  the `mode` item: `rig-mode.sh test` (the action the `mode=EVENT→FORGOT` line names) restores
  scenes, Studio Mode, burns AND painter in one shot. A dedicated read-only scene/studio probe is a
  followup, not this lane.
- **Two-pass expectation (mic/painter are EVENT-gated).** `measurement-audio-alert-watchdog`
  SKIPs in EVENT mode and the painter is stopped in EVENT, so on the FIRST pass — run the moment
  the owner hands the rig back, while it is usually still in EVENT — `mic` and `painter` read
  UNKNOWN. Switch the rig back with `rig-mode.sh test` (the `mode=EVENT→FORGOT` action) and re-run
  the check: `mic`/`painter` only verify once the rig is in TEST. The muted-mic that motivated the
  ticket is caught on the SECOND pass.
- **Multi-box items are strict (no masking).** For `burns` (strih+stream) and `pins`
  (strih+stream+imag) the item is OK only when EVERY probed box is OK; a single unreadable box
  makes the item UNKNOWN (never masked by an OK sibling — the honesty the `neoverené` line needs).
  FORGOT still dominates. Single-node verdict-watchdog items keep the watchdog's own SKIP-defer
  semantics (an away/down peripheral box like imag/resolume contributes nothing, so at least one
  HEALTHY core box reads OK) — matching how the watchdogs themselves defer a SKIP to network-reach.
- **Version items need fleet node specs.** `dantesync-version-gate.sh` /
  `camera-box-version-gate.sh` REFUSE (exit 1 → UNKNOWN) with no `--linux/--win` nodes. The
  dantesync item runs the gate with **`--fleet`** (issue 1372): the node set is the ONE declared
  dantesync fleet (`scripts/lib/dantesync-fleet.sh` — every camera, dev1, the OBS boxes, mbc,
  fohabl; fohabl's credential from the dev1-local cred file, UNKNOWN without it). The cambox item
  still builds the active-cam `name=root@ip` spec by sourcing `camera-set.sh` in a `$()` subshell and
  uses `--no-main-pin` (relative peer parity = uniform build, no origin/main read).
- **The alert-watchdogs log `verdict=`/`reachable=` to STDERR (via `log()`), not stdout** — the
  pure `*_decision.py` `verdict=` stdout is captured into a shell var inside the watchdog and never
  reaches the terminal. The orchestrator therefore captures each probe with `2>&1` (merged) and the
  parser reads the merged text. Dry-run exit is ~always 0 (2=bad arg, 3=require_tools missing); the
  verdict lives in the log line, never the exit code — that is why the watchdog items are
  `verdict`-kind (parse the line) and only the OBS/version probes are `exit`-kind.
- **Reuse, never re-derive.** Every item runs an EXISTING probe. When a probe's dry-run verdict
  vocabulary changes, update the `good`/`forgot` token sets in `ITEMS` (rig_dev_handover_decision.py)
  and the fixtures in the test — do NOT reimplement the probe's grading here.
- The `#1310`/`#1307`/`#1299` watchdogs may be absent on an older base — a MISSING probe script is
  the RC_MISSING sentinel → UNKNOWN, so the orchestrator is forward-compatible (it picks a probe up
  the moment it exists).
