---
name: genlock
description: >
  Genlock OBS build — current deployment state on strih+stream, monorepo direction,
  fork history. Load when working on genlock (#8/#11), vendored OBS/DistroAV,
  drift-guard, or anything touching the broadcast OBS on strih/stream.
---

# Genlock

## #257 — PRODUCTION-SAFE HARD-LOCK (the CURRENT state; supersedes the env model below)

**GOTCHA (2026-07-18, live-event #797 debugging): the vendored libndi runtime IGNORES
`ndi-config.v1.json` — transport cannot be forced via config on this build.** Writing
`{"ndi":{"rudp":{"recv":false,"send":false},"unicast":{...false},"multicast":{...false}}}` to
`~/.ndi/` (receiver side, imag) AND `/root/.ndi/` (sender side, cam box service runs as root) had
ZERO effect — wire stayed pure UDP after full process restarts on both ends. Do not burn event
time retrying config-based transport switches; the post-event paths are on camera-box#797
(NDI_CONFIG_DIR env? version-specific keys? SDK update in vendor/). Context: NDI RUDP sender-side
per-connection degradation to exactly ~50/60fps against the LINUX receiver only (Windows strih
receiver clean, sender-wire capture proved 14-15ms burst pacing originates in the sender's lib).


**The genlock build is hard-locked and ENV-FREE. There is NO `OBS_GENLOCK_*` / `OBS_BURN_*` env any
more** — the old env knobs were removed in #257. The current model:

- **Render tick + ts-align are ALWAYS ON in the build** (`obs-video.c genlock_tick_enabled` /
  `obs-source.c genlock_ts_align_enabled` just `return true`). No `OBS_GENLOCK_WALL_CLOCK` /
  `OBS_GENLOCK_TS_ALIGN`. The proof is the OBS-log line `genlock: … render tick ENABLED` (drift-guard
  capability marker + the launch-wrapper log-verify key on it).
- **Genlock latency is a BUILD CONST: 3 ms, floor 3** (`GENLOCK_LATENCY_MS_DEFAULT`/`_MIN` = 3 in
  `obs-source.c`, mirrored in `src/probe/genlock.rs`). No `OBS_GENLOCK_LATENCY_MS` / `_RESERVE_MS`.
  The PER-SOURCE override is the DistroAV UI int **"Latency (ms)"** (min 3, max 2000, default 3),
  applied at runtime via `obs_source_set_genlock_latency_ms` (clamps 1→3, 0→3). preload is fully
  internal/auto (no `OBS_GENLOCK_PRELOAD_FRAMES`).
  - **#292 — the drop-cap MUST budget the held depth at the SOURCE ARRIVAL fps, NOT the canvas
    output fps.** The ts-align deadline (`present_ts = wall_now − latency_ms`) holds every queued
    frame younger than `latency_ms`, so the FIFO fills at the source ARRIVAL rate. **At the time
    #292 was fixed (pre-#459), the stream box received a 60 fps NDI feed from strih into a 30 fps
    canvas** (the #11 60→30 strih→stream topology) — that was the concrete 60-into-30 case in play.
    **Topology v2 (#459) moved this same shape onto STRIH itself:** strih is now cut-to-stream only
    at 30fps, but its OWN camera ingests (`NDI cam5`/`NDI cam1`/`NDI cam3`) are still 60fps NDI
    feeds arriving into strih's 30fps canvas — so the identical 60-into-30 case the #292 fix guards
    now applies to strih's camera ingests instead of stream's `NDI 2ME PGM`. The fix (below) is
    UNCHANGED and still correct either way: 1000 ms parks ≈60 frames, not the 30 the canvas rate
    implies. The old
    `genlock_source_drop_cap` budgeted `latency_frames` at `genlock_video_fps()` (= canvas fps),
    undercounting ~2x → the overrun force-drain capped the delay at **~450 ms** (≈27-29 frames near
    the `MAX_ASYNC_FRAMES=30` floor at 60 fps) — the operator could not delay the stream the ~1 s
    needed to A/V-align to the late mastered audio. Fix: budget at `GENLOCK_MAX_SOURCE_FPS = 60`
    (the rig's max source rate; cameras+strih render 60), honouring the canvas rate too if ever
    higher. 2000 ms @ 60 fps = 120 frames + RESERVE 4 = cap 124 < abs-max 132 (`GENLOCK_PRELOAD_MAX`
    128 unchanged — already sufficient; the binding constraint is depth+reserve, not abs-max). Pure
    helper `genlock_latency_depth_frames(latency_ms, canvas_num, canvas_den)` in `src/probe/genlock.rs`
    mirrors the C; the C divide `(latency_ms*60+500)/1000` == `ms_to_frames(ms,60,1)`. Guard
    `drop_cap_budgets_at_source_arrival_fps_in_vendored_source` pins the C `#define
    GENLOCK_MAX_SOURCE_FPS 60` + the budget term. **DELIVERY (set 1000 ms, measure A/V align on
    stream) is a SUPERVISOR rig step at the coordinated OBS rollout** — the unit test is the
    code-level proof. Do NOT reduce the latency — the ~1 s is INTENTIONAL (aligns video to the
    1 s-late mastered audio); the fix RAISES the achievable max, never lowers latency.
- **DistroAV NDI source UI is a HARD WHITELIST** (`ndi_source_getproperties`): exactly four props —
  `PROP_SOURCE`, `PROP_GENLOCK_FIFO` (Genlock, default ON), `PROP_GENLOCK_LATENCY_MS_SRC`
  (Latency ms), `PROP_BURN` (Measurement burn, default OFF). Every other DistroAV knob is removed
  from the UI and FORCED to a certified value (`force_genlock_certified_settings` ← the
  `GENLOCK_FORCED_SETTINGS` const table, the complement of `GENLOCK_WHITELIST_PROPS`).
  **`PROP_BANDWIDTH` is forced to `PROP_BW_HIGHEST` for every genlock source by default** — there
  is NO "reduced-bandwidth for off-program sources" mode anywhere in this pin (checked while
  ruling out a #707 hypothesis, 2026-07-13). The ONE exception is `PROP_GENLOCK_MONITOR` (#501,
  `ndi-source.cpp` line ~394): a source explicitly flagged `genlock_monitor=true` gets
  `PROP_BANDWIDTH` narrowed to `PROP_BW_LOWEST` INSTEAD — but that flag is set on a SEPARATE,
  DEDICATED "MV Cam N" twin-scene source (imag-nb only, `scripts/imag_scenes.py`), never on the
  regular "Cam N" program-feeding source itself; the twin exists purely to feed imag-nb's built-in
  multiview cheaply (its own render-budget fix, unrelated to program switching) — a "Cam N" scene's
  own NDI source is ALWAYS `PROP_BW_HIGHEST`, whether it's currently in program or not, and this
  whole twin-scene mechanism is **imag-nb-specific and does not exist on strih at all** (issue
  #730, still open, tracks building strih's own per-camera multiview — it doesn't have one yet).
  So there is no bandwidth-mode-switching mechanism to trigger a post-switch "ramp to full rate" on
  strih's own camera ingests, at the code level, today.
- **Measurement burn is a per-source `genlock_burn` bool, runtime, NO restart** — toggled over OBS
  WebSocket `SetInputSettings genlock_burn` (`scripts/obs_burn_filter.py add|remove`, driven by
  `scripts/rig-mode.sh test|event`). libobs stores it (`obs_source_set/get_genlock_burn`); the QR
  burn filter reads `obs_source_get_genlock_burn(parent)` each render. run_id/corner come from the
  box's **host role** (strih 911002/bottom-left, stream 911004/bottom-right — no `OBS_BURN_RUN_ID/
  _CORNER`), qr size canvas-relative auto (no `OBS_BURN_QR_PX`).
- **`launch-obs-genlock.sh` is env-free** — relaunch = clear sentinels → Start-Process cwd=bin\64bit
  → log-verify `render tick ENABLED` + DistroAV. No PEB env check (there is no env to carry). The
  `--mode test|event` is gone (the burn is a WS toggle, not a relaunch).
- **drift-guard #246 burn facet** now means "no prod source has `genlock_burn=on`" (read over WS),
  not "no `OBS_BURN_*` in Machine env". `genlock_wall_clock=1` is a build-default sentinel proven by
  the capability marker.

The `#235` env model and the `STALE-ENV TRAP` notes below are HISTORY (pre-#257) — there is no
genlock env to lose any more. Tests: `tests/genlock_preload.rs`, `tests/distroav_genlock_lockdown.rs`,
`tests/launch_obs_genlock.rs`, `tests/rig_mode.rs`, `tests/drift_guard.rs`, `tests/burn_payload_parity.rs`.

**PLAYBOOK HYGIENE (when you kill an env var / identifier):** grep the WHOLE playbook —
`grep -rE 'OBS_GENLOCK_|OBS_BURN_' .claude vendor/README.md` (and the same for any newly-killed
name) — and historicize/remove EVERY operator-facing instance, not just the obvious skill. The #261
no-env rewrite covered genlock + obs-ops + drift-guard + vendor/README, but `.claude/skills/e2e/SKILL.md`
still had active `$env:OBS_BURN_QR=1` launch steps (filed #262) — a killed knob hides in the skill you
didn't think to open.

## (HISTORY, pre-#257) Genlock latency env knobs — ALL REMOVED in #257

⚠️ **These env vars NO LONGER EXIST.** Latency is now a build const (3 ms, floor 3) with the
per-source override in the DistroAV source UI — see the #257 section at the top. This section is kept
only to explain the lineage; **never set any `OBS_GENLOCK_*` env — there are none.**

Pre-#257, genlock latency went through ONE env knob in MILLISECONDS (#235), which had superseded an
earlier confusing dual model (`OBS_GENLOCK_PRELOAD_FRAMES` whole frames + `OBS_GENLOCK_RESERVE_MS`
ms, reserve overriding preload only under TS_ALIGN):

- `OBS_GENLOCK_LATENCY_MS=N` *was* THE held latency in ms — release deadline `wall_now − N·1e6` (#184),
  implying ts-align ON. `OBS_GENLOCK_RESERVE_MS` *was* a back-compat alias; `OBS_GENLOCK_TS_ALIGN` and
  `OBS_GENLOCK_PRELOAD_FRAMES` *were* the older gates. **All four were deleted in #257** — render tick
  + ts-align are now build defaults and the latency is the UI int (floor 3).
- **preload is internal** (auto-derived FIFO depth = 1 frame for jitter/dropout resilience,
  latency-free under the ms deadline so the #110 0-loss floor holds) — unchanged, still true.
- **Display:** the OBS startup + audit log show `genlock: latency = N ms (≈ M frames @ Ffps)` — MS
  PRIMARY, frame-equivalent in PARENS (this log format is unchanged across #257). Pre-#257 the latency
  was env-set and the DistroAV source props showed only a READ-ONLY `Genlock latency = N ms` label;
  post-#257 that prop is the EDITABLE "Latency (ms)" int (min 3) — a user sets ONE ms value in the UI
  (not an env) and never reasons about preload-vs-reserve precedence.

Resolution + display mirrored & unit-tested in `src/probe/genlock.rs` (resolve_latency_ms /
ms_to_frames / genlock_auto_preload / format_latency_label) with vendored-source guards keeping the
C (`vendor/obs-studio/libobs/obs-source.c` genlock_latency_ms) + DistroAV in lock-step.

**GOTCHA — a `genlock:` log line has THREE consumers; change all of them together.** When you edit a
`genlock:` line in `obs-source.c` (e.g. the #235 rename from `sub-frame jitter reserve = N ms` to
`latency = N ms (≈ M frames)`), you MUST update in the SAME PR: (1) the `tests/genlock_preload.rs`
vendored-source guard string, (2) `scripts/launch-obs-genlock.sh` (#128 wrapper) log-verify regex,
(3) `scripts/drift-guard.sh` `genlock_capability_from_log` regex (which keys on the build-unique
`genlock:` lines to catch a stock-OBS #119 wrong-build). Missing any one silently breaks the launch
verify or capability detection while every other test stays green.

## (HISTORY, pre-#257) Sub-frame ms reserve (#184) + the stale-env launch trap — env removed

⚠️ **No genlock env any more (removed in #257).** Kept for lineage only; **never set an
`OBS_GENLOCK_*` env.**

Pre-#257, `OBS_GENLOCK_RESERVE_MS=N` (the #235 latency alias) switched the genlock ts-align release
deadline from the whole-frame `preload·interval` (=33ms@30fps) to `wall_now − N·1e6` (ms-granular);
`latency_ms=0` was the #136 frame path verbatim. Validated zero-loss at 3 ms on BOTH hops (strict
recording-verdict `overall_pass`, FIFO audits `latency_ms=3 reserve_ms=3`, 0 new underruns during
active feed). Prod ran at 3 ms via the Machine env; **#257 cleared all genlock env on both boxes and
made render tick + ts-align build defaults with the 3 ms floor** — so prod still runs at 3 ms, now from
the build const + per-source UI int, NOT env. Rollback DLL (pre-#184 whole-frame build):
`C:\obs-backup\pre-184\obs.dll` (`cdce8c3a…`).

The pre-#257 **STALE-ENV TRAP** (a win-* MCP shell inheriting a stale env snapshot, so an OBS launched
from it silently ran the whole-frame path with no latency line) is **moot now — there is no genlock
env to inherit.** The lasting lesson survives the env: a running OBS's genlock state is read from the
OBS log (`genlock: … render tick ENABLED` + `genlock: latency = N ms` once a genlock_fifo input is
live), NEVER from an env read. NB the FIFO audit's `underruns=` is CUMULATIVE per OBS process — a huge
value can be IDLE accumulation between runs; what matters is the DELTA during active feed (0 = clean).

**dev1 ⇄ rig transfers:** dev1 file-drop (`:8788`) is NOT reachable from the rig. dev1→stream
binary push works via SMB admin share `smbclient //10.77.9.204/C$ -U "newlevel%newlevel"` (newlevel
is admin). strih→stream SMB works (net use \\10.77.9.204\C$); strih's own C$ DENIES dev1.

**recording-verdict cam1 contiguity:** the STREAM-ONLY single-recording cam1 read is SOFTENED and
may OVER-COUNT real_drops (#133/#216) — supply BOTH `--strih` + `--stream` for the STRICT cam1
verdict (the both-hops #184 run: softened stream-only = 37 cam1 drops, strict = 0).

## #272 — why the 3ms floor is real (not the DanteSync clock), + jitter-log tooling

Full decision record: `docs/genlock-latency-floor-rationale.md`. One-line answer: the reserve
absorbs frame-**arrival** jitter (NDI receive + render-tick + CPU scheduling), NOT clock
inaccuracy — a µs-disciplined shared clock does not touch any of those three. Proof: cam1→strih
measures **8.1ms** jitter (`obs-source.c:4674`) on the SAME DanteSync clock as strih→stream's
**1.6ms** — same clock, 5× different jitter, so the clock is not the bottleneck. `reserve_ms=3`
was empirically validated at zero-loss (#184/PR #224), not picked arbitrarily; the worst spike
(28ms head-skew) was root-caused to CPU-scheduling contention, not the clock (#289/PR #304, fixed
by core pinning).

**New tooling to analyze a `genlock-fifo audit` log window (any future latency investigation,
not just #272):** `camera_box::jitter_audit` (Tier-0 pure, `src/jitter_audit.rs`) parses the
periodic audit line into an `AuditSample`, groups by source, and `summarize`s a captured window
into DELTA loss counters (`underruns`/`holds`/`dropped_due`/`late_holds`/...) + the
`ts_head_skew_ms` jitter distribution. Thin CLI: `genlock-jitter-report --file <obs.log>` (or
stdin) — prints one row per source, exits 2 (fail closed) if no audit lines are found. The
parser is whitespace-token-based (`key=value` tokens matched by exact key name); it does NOT
need to model the log's decorative `(≈N frames @ Ffps)` / `(=N ms)` / `(re-arm@N)` /
`(#70/#97/...)` fragments — none of those contain a RECOGNIZED `key=`, so they're silently
skipped. Reuse this parser for any future "read the audit log and compute a delta" need instead
of re-deriving a sed/regex one-off.

**GOTCHA — the #797 "phantom 50.1 fps" (2026-07-18): NEVER divide an audit-counter delta by a
wall-clock sleep.** The audit line appends every **~5.017 s** (not 5.000). A hand-rolled one-off
(`/tmp/imag-diag.py`) snapshotted the LATEST line's `received=`, slept 6 s, snapshotted again and
divided by 6 — a 6 s wall window almost always spans EXACTLY ONE new audit tick (+301 frames at a
true 60 fps), so the meter printed **301/6 = 50.17 ≈ "50.1" at every true-60 source, always**
(cross-check that unmasked it: a ~43 fps Resolume feed showed "35.8" = 215/6). Two days were spent
hunting a phantom "OBS receive caps at 50 while ndi-recv-probe gets 60" — the probe-vs-OBS
differential compared a correct meter against this broken one. Rule: rate from a fixed-cadence log
counter = delta ÷ **the matched lines' OWN log timestamps** (or count whole ticks × 5.017), i.e.
use `jitter_audit`/`genlock-jitter-report` per the paragraph above — or, for receive rate, the
`recv-timing #797` line (measured inside the DistroAV loop with steady_clock; lands ONLY in OBS's
own log file, NOT in `/tmp/imag-obs-start.log`). Full post-mortem: #797 (retraction comment).

**Still open (empirical, not this ticket):** going BELOW 3ms needs a floor-varied OBS *build*
(the const can't vary at runtime post-#257) + a live recording + `genlock-jitter-report` on the
captured log — a build-matrix change, supervisor/user-driven runbook in the doc's §7.

## #859 — a DEEP per-source latency changes which FIFO branch runs (backlog threshold is latency-relative)

The backlog-storm branch in `obs-source.c` (`async_frames.num > threshold && due > 0` → re-lock to
the newest due frame, erasing every jumped frame into `dropped_due`) used a bare
`GENLOCK_QDEPTH_RELOCK 6`, calibrated on the assumption stated in its own comment: *"steady depth is
~1-2 at any skew, the boundary paces arrivals"*.

**That assumption only holds for a SHALLOW source.** The held latency is `wall_now - reserve_ms`, so
a source pinned deep — the stream box's `NDI 2ME PGM` runs **923 ms** so the A/V controller can
align against the mbc's 1 s mastering — has a steady depth of ~28 and exceeded the bare 6
permanently. Symptoms, all on the deep source only:

- `relocks` increments **once per frame** (this is what #796 reported as "useless as a health
  signal" — it was actually the branch genuinely firing every tick)
- `holds` and `dropped_due` advance **in lockstep** on every arrival-jitter excursion to `due == 2`:
  one frame erased, the next tick repeats the last — a paired duplicate/skip
- measured as **+59 duplicates / +57 skips** injected into the strih→stream hop, against **2
  duplicates in 9626 frames** on cam→strih, whose sources all sit below 6 and report `holds=0`

Since #859 the threshold is `genlock_backlog_relock_qdepth()` = the depth that source's own
configured latency implies (using the SOURCE rate — a 60-into-30 input queues two entries per canvas
interval) **plus the unchanged margin 6**. Sub-half-frame sources (the 3 ms global default, the 3 ms
imag contract) are byte-identical to before. The decision is Tier-0 unit-tested in
`src/genlock_backlog.rs`; the C and `src/probe/genlock.rs` both derive from it and
`tests/genlock_release_cadence.rs` asserts the C actually CALLS it and that the margin stays 6.

**Reading the audit for this class of bug:** compare `depth` against the latency's implied frames
(`latency_ms / 33.3` at 30 fps), not against a fixed number — and treat `relocks` climbing at frame
rate on a *healthy* source as a threshold bug, not a transport problem.

## #803/#912 — per-source ASRC clock-drift servo, ALWAYS ON BY DEFAULT (build const, no toggle)

`vendor/obs-studio/libobs/media-io/asrc-compensator.{h,c}` is a per-source ASRC (async
sample-rate conversion) servo — continuously nudges a source's swresample compensation (ppm) to
hold its audio timeline on the video master clock, using `genlock_wall_now_ns()` (the same
wall-clock basis the video FIFO release uses) as the reference, NOT the source's own presentation
timestamps. Constants (`ASRC_MAX_PPM 300`, `ASRC_MAX_SLEW_PPM_PER_S 5`, `ASRC_TIME_CONSTANT_S 20`,
`ASRC_MIN_LOCK_S 5`) MUST stay numerically identical to their Rust mirror `src/asrc_bench.rs` (the
pure closed-form simulation, `.claude/rules/asrc-bench-harness.md`).

**#803 (PR #911) shipped the servo wired to `struct obs_source`'s `asrc_enabled` bool but
NOTHING in the vendored tree ever called `obs_source_set_asrc_enabled()`** — it was permanently
inert. Worse: **it shipped with ZERO vendored-source lock-step anchors** (no
`tests/genlock_preload.rs` guard, no pwsh gate in either `windows-genlock{,-fast}.yml`) despite
this being the single most emphasized convention in this skill (see "CI exact-anchor guards live
in YAML too" below) — a genuine gap in that session, not a one-off. **Lesson: never assume a
vendored feature has its lock-step anchors just because this repo's convention says it should —
grep `tests/genlock_preload.rs` and both workflow ymls for the issue number before treating "no
anchor" as "nothing to guard."**

#912 fixed the inert-by-default problem the same way #257 fixed the genlock env-knob problem: made
it a BUILD DEFAULT (`obs_source_create_internal()` sets `source->asrc_enabled = true;`, no env, no
per-source opt-in), kept the setter/getter EXPORTed as an optional override path only (nothing
calls it), and added the FIRST lock-step anchors for this feature —
`tests/genlock_preload.rs::vendored_source::asrc_default_on_present_in_vendored_source` +
`::distroav_source::windows_genlock_workflows_gate_on_asrc_default_on` + a pwsh gate in both
workflow ymls. #912 did NOT retroactively anchor the REST of #803 (the servo math, the
process_audio() wiring) — that remains unguarded; a future ticket touching that code should add
its own anchors rather than assume they exist.

No source-class exclusion / GUI checkbox was added even though the issue explicitly allowed one:
`asrc_process_audio()`'s `raw_advance_s` is `frames / samples_per_sec` and `master_block_s` comes
from wall-clock, not from source PTS — a media/VLC source's seek changes content, not the real-time
cadence of `process_audio()` calls, so the existing clamp+slew already bound any transient fallout.
Non-audio sources never call `process_audio()` at all (pure no-op for them).

## Deployed State (strih + stream, since 2026-06-13)

Both production broadcast OBS boxes upgraded in-place to the camera-box genlock build.

| | strih (10.77.9.202) | stream (10.77.9.204) |
|---|---|---|
| OBS version | 32.1.2 | 32.1.2 |
| Build SHA | cf7b0606 | cf7b0606 |
| Genlock active | YES | YES |
| Genlock env | none — build default (#257) | none — build default (#257) |

(LIVE 2026-07-16: **strih obs.dll = the #776 MV-divisor build `95709867c` (EE9BA019…)** — multiview
renders every tick (30fps MV; render 30fps/21.5ms/0% skip, fits the 33ms budget with headroom; the
2026-07-15 "doesn't fit" conclusion was CONFOUNDED by the bare-exe launch problem, not the build);
**stream obs.dll = the #767 keep-alive build `dede91825` (E6887854…)**. Verify live via
`GENLOCK_BUILD_SHA.txt` + `Get-FileHash` — NEVER from a stale SHA stamp alone; Copy-Item preserves
source mtime, so a rollback is invisible in timestamps. The version/SHA row above is the 2026-06-13
baseline; the current deployed bytes are the #257-lineage builds —
see `docs/autopilot-log.md` for the live obs.dll / distroav.dll SHAs and the drift-guard `--compare`
manifest check. Genlock is no longer gated by any env: it is a build default since #257.)

**Genlock is ACTIVE:** both boxes log `genlock: wall-clock-slaved render tick ENABLED` at OBS launch.
stream's live `NDI 2ME PGM` input has `genlock_fifo=True` → production strih→stream hop is genlocked.

**Measured on production (2026-06-13, synth→strih→stream, strict gate, 120s):**
VERDICT=PASS, 0 dropped on both hops (0/3556 and 0/3535 single-copy), p99 77/92 ms.

Camera→strih hops: genlock tick active but camera ingests are NOT genlock_fifo yet
(camera-box senders must wall-pace first, #11).

**Backups (instant rollback):** `C:\obs-backup\2026-06-13\` on each box.
Rollback = stop OBS, robocopy backups back over `C:\Program Files\obs-studio` + the
ProgramData distroav, clear `%APPDATA%\obs-studio\.sentinel\*`, relaunch.

## FULL-BUNDLE in-place deploy runbook (#726 session, 2026-07-14) — the AHK-watchdog gotcha

When to full-bundle vs obs.dll-only: a **struct change** to `obs_source` (e.g. #726's
`genlock_last_known_n`) is technically ABI-safe as an **obs.dll-only** swap (distroav + the frontend
hold `obs_source_t*` opaque handles, never `obs-internal.h`; proven live — the new obs.dll loads the
OLD distroav.dll fine, genlock FIFO works), so a hot-swap RUNS. **But** a full windows-genlock build
from dev HEAD also rebuilds `obs64.exe` + `libobs-opengl.dll` (git-describe/`__DATE__` embedded),
which the boxes' last full-bundle deploy predates — so obs.dll-only leaves those STALE and
`drift-guard --compare` (manifest per-component check) reports the mismatch. If the ticket/spec says
"full bundle, NO DRIFT", deploy the whole bundle, not just obs.dll.

Procedure (per box, ~10 files actually differ but deploy the whole stage for NO-DRIFT):

1. `airuleset.py`-serve or `python3 -m http.server --bind <dev1 LAN>` the bundle as ONE zip
   (`zip -rX bundle.zip . -x '*.pdb'` — PDBs are never deployed; ~172 MB). Backup the differing
   components to `C:\obs-backup\<date>\*.pre-<tag>` FIRST.
2. Download + `Expand-Archive` on the box while OBS keeps running (no impact).
3. **STOP THE AHK WATCHDOG BEFORE KILLING OBS** — `NL_STARTUP.ahk` (a "Safe loop" `AutoHotkey64`
   process, `D:\_APPS\NL_STARTUP.ahk`, on **strih** — stream has NONE) auto-respawns obs64. If you
   robocopy while it's alive, it relaunches obs64 mid-copy, which re-locks `data/` + `obs-plugins/`
   files → `robocopy` exit code **11/10** (bit 8 = FAILURE, some files not copied), silently leaving
   DRIFT. `Get-Process AutoHotkey64 | Stop-Process -Force` first; **restart it at the END** with the
   same cmdline (`Start-Process 'C:\Users\newlevel\AppData\Local\Programs\AutoHotkey\v2\AutoHotkey64.exe' -ArgumentList 'D:\_APPS\NL_STARTUP.ahk'`).
   Its `app1_run` keeps obs64 (`OBS Studio.lnk`) alive; `app3` Resolume, `app6` tally — killing it
   briefly doesn't stop those (already running), only removes the watchdog until you restart it.
4. Kill obs64 (+ `obs-browser-page`), clear `%APPDATA%\obs-studio\.sentinel\*`.
5. Surgical overwrite-keep-extras (preserves 3rd-party plugins — NEVER `/MIR`/`/PURGE`):
   `robocopy <stage>\bin\64bit  "<root>\bin\64bit"  /E /XF *.pdb /MT:16`,
   `robocopy <stage>\data       "<root>\data"       /E /MT:16`,
   `robocopy <stage>\obs-plugins\64bit "<root>\obs-plugins\64bit" /E /XF distroav.dll /MT:16`
   (distroav lives in **ProgramData**, not Program Files — `/XF` it to avoid a shadow copy that
   drift-guard's `distroav_dll_paths` check flags). Copy `BUNDLE_MANIFEST.json` +
   `GENLOCK_BUILD_SHA.txt` to `<root>\` (so `drift-guard genlock_build_sha` reads the new build).
6. Verify NO drift: `robocopy … /L` and grep for lines NOT matching `*EXTRA` — every genuine
   copy-candidate must be gone. **`data\obs-plugins\win-dshow\obs-virtualcam-module64.dll` may still
   show as `Newer` and fail (bit 8) — it's held by the Windows Camera Frame Server / any
   camera-enumerating app even with OBS down.** Check `Get-FileHash` box-vs-stage: it is
   content-IDENTICAL build-to-build, so a bit-8 on it alone is BENIGN (newer timestamp only, not real
   drift) — do not chase it.
7. Relaunch = **unconditionally** kill → clear sentinel → `Start-Process` (never gate the
   sentinel-clear on "if not running" — on strih the AHK respawn races you and the guard skips the
   clear, so obs64 hangs on the "Crash Detected" modal at ~93 MB working-set with no genlock line;
   the tell is a fresh log whose last line is `Crash or unclean shutdown detected` and NO
   `genlock: … render tick ENABLED`). Poll for `render tick ENABLED` in the newest log.
8. `drift-guard --compare host=<box> … manifest=<bundle>\BUNDLE_MANIFEST.json obs_dll_sha256=<full>
   distroav_dll_sha256=<full> genlock_build_sha=<sha> genlock_capability="genlock: wall-clock-slaved
   render tick ENABLED" ndi_input_latency="<inp>=0,…"`. **Supply `ndi_input_latency`** (read the
   DistroAV `latency` field via `obs_phase2._rpc(ws,"GetInputSettings",…)['inputSettings']['latency']`,
   0=Normal) or the run exits **11 (UNKNOWN/incomplete), not 0** — that is NOT drift, just a value you
   didn't gather. Reopen the strih Multiview after relaunch via WS
   `OpenVideoMixProjector {videoMixType:OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW, monitorIndex:1}`
   (strih monitors: `U27P2G6B`(0)=UI, `SyncMaster`(1)=multiview panel).

### Doing the full-bundle deploy over the `win-*` MCP (confirmed live #707, 2026-07-14)

Confirmed nuances when the deploy is driven from a `mcp__win-strih__Shell` / `mcp__win-stream-snv__Shell`
(NOT the ssh wrapper):

- **Each MCP Shell call is a SEPARATE elevated PowerShell process** — it IS admin (robocopy to
  `C:\Program Files` works), but `Start-Job` / any background job dies the instant the call returns
  (the parent shell exits). So do NOT background box-side work with `Start-Job` and poll it in a
  later call — the job is already gone. Either run the whole robocopy INLINE in one elevated call
  (raise the Shell `timeout` — 240 s honored) OR use a detached `Start-Process powershell -WindowStyle
  Hidden -File <script>` that writes a status file, and poll THAT file.
- **The `data\obs-plugins\win-dshow\obs-virtualcam-module64.dll` lock hangs robocopy FOREVER.** It is
  held by the Windows Camera Frame Server even with OBS down; robocopy's default `/R:1000000 /W:30`
  retries it endlessly (the run "stalls" with no error). ALWAYS deploy `data\` with
  `robocopy … /R:2 /W:2 /XF obs-virtualcam-module64.dll` — the file is content-IDENTICAL build-to-build
  (verify `Get-FileHash` box-vs-stage), so excluding it is zero drift. (This is the #726 gotcha; the
  live fix is the `/XF` + short retry cap.)
- **Serve the bundle from dev1 with `run_in_background: true`** (not `nohup … &` in a plain Bash call —
  that gets SIGTERM'd on return; exit 144). dev1 rig-subnet IP is `10.77.9.165`; boxes reach it there.
- **drift-guard `--compare` build facets vs runtime pins.** With ONLY the box SHAs
  (`obs_dll_sha256`/`distroav_dll_sha256`/`genlock_build_sha`/`genlock_capability` + `manifest=`) the
  BUILD facets go `OK` but the run exits **11 (INCOMPLETE — NOT drift)** because 4 runtime pins are
  UNKNOWN. For a clean **exit 0** also supply: `distroav_version=6.2.1`, `ndi_runtime=6.3.2.0`
  (compare is `>= 6.3.0`, lenient), `distroav_dll_paths=C:\ProgramData\…\distroav.dll`, and
  `ndi_input_latency="<inp>=0,…"`. Gather the per-source latency via WS — `obs_phase2._conn(host,pw)`
  then `GetInputList` → for each `inputKind=="ndi_source"` with `genlock_fifo=true`,
  `GetInputSettings`→`inputSettings.latency` (0=Normal). WS password is in local memory
  (`rig-obs-ws-credentials`), `NDI_RUNTIME_DIR_V6=/usr/lib/ndi`.

## Bundle version integrity (EPIC #125)

**On-box build identity in the OBS title (#152):** the window title is composed in
`OBSBasic::UpdateTitleBar()` (`vendor/obs-studio/frontend/widgets/OBSBasic.cpp`) — after
the OBS version (`GetVersionString`, which already carries the git-describe sha) it appends
` - newlevel.media build <YYYY-MM-DD>`. The date is the compiler `__DATE__` reformatted to
ISO by the file-local `NewlevelBuildDate()` helper just above `UpdateTitleBar()` (uses only
std::string + std::ostringstream — the SAME `<string>`/`<sstream>` dependency as the existing
`stringstream name;` in `UpdateTitleBar`, so no new includes → near-zero frontend-compile
risk; note `<sstream>` is `#ifdef _WIN32` in OBSBasic.cpp but the genlock build target is
Windows-only; prefer `__DATE__`
over CMake/config-header date plumbing since the full frontend build is 150-min and not
locally compilable). Guard = `tests/obs_titlebar_newlevel.rs` (NON-probe, default-feature
Linux test → runs Tier-0; RED→GREEN provable locally by running the built
`target/debug/deps/obs_titlebar_newlevel-*` against the source) + the lock-step pwsh
source-anchor gate in BOTH `windows-genlock{,-fast}.yml` (the FAST path doesn't build the
frontend but still source-text-gates the token, same as the #276/#278 OBSProjector gate).

**#313 — that #152 `NewlevelBuildDate()` parse CRASHED OBS at startup (regression).** It
`d.substr(7, 4)`-ed `__DATE__` with NO size guard, so a non-11-char compile-date string threw
`std::out_of_range` (MSVC `std::_Xran` "invalid string position") out of `UpdateTitleBar()`
during `OBSBasic` construction → OBS aborted on EVERY launch. **A title-bar helper must NEVER
throw.** Fix: the parse is now the pure, OBS/Qt-free `newlevel_iso_date(const std::string&)`
in `vendor/obs-studio/frontend/widgets/NewlevelBuildDate.hpp`, guarded
`if (compileDate.size() < 11) return "unknown";` before any indexing/`substr`.
`NewlevelBuildDate()` keeps the pinned anchor line `const std::string d = __DATE__;` then
`return newlevel_iso_date(d);` — so the two `windows-genlock{,-fast}.yml` source gates +
`tests/obs_titlebar_newlevel.rs` stay GREEN with NO lock-step change.
**THE LESSON — the OBS FRONTEND (`OBSBasic.cpp` + all of `frontend/`) compiles ONLY on the
150-min `windows-genlock.yml`; neither the fast build nor normal PR CI compiles it.** A frontend
C++ bug is therefore INVISIBLE to PR CI until the supervisor's full rebuild. Extract ANY
non-trivial frontend logic into a pure header and behaviorally unit-test it off-rig with the #293
harness pattern — **which now covers C++ too**: `tests/obs_titlebar_newlevel_parse.rs`
compiles+runs `NewlevelBuildDate.hpp` via `c++ -std=c++17` (the `obs_display_budget.rs` twin uses
`cc -std=c11`). Run it Tier-0 with `cargo test --no-run --test obs_titlebar_newlevel_parse` then
execute the built `target/debug/deps/obs_titlebar_newlevel_parse-*` directly (no probe, no bypass).

Two LAYERS guard "the deployed stack is the build we think it is":

- **drift-guard (#45)** — marketing versions + critical settings (OBS 32.1.2 / DistroAV
  6.2.1 / fps / genlock gate / input latency / canonical plugin path).
  `scripts/drift-guard.sh --check-pins` (CI) + `--compare` (live box). Pins live in
  the `vendor/README.md` version + settings tables. The version+settings facet alone
  cannot catch stale-BYTES-of-the-right-version (that was #119: a pre-#97 DistroAV of
  version 6.2.1 → preload inert) — **#122 (below) closes that** with a per-component
  BUILD-SHA + capability check.
- **per-component SHA manifest (#120)** — the windows-genlock build emits
  `stage/BUNDLE_MANIFEST.json` via `scripts/genlock-manifest.sh` (unit-tested
  `tests/genlock_manifest.rs`): `components[]` = each rebuilt component's pinned
  version + vendored subtree commit (DistroAV version cross-checked vs
  `vendor/distroav/buildspec.json`, same source-of-truth as drift-guard) + NDI
  `min_version`; `files[]` = every shipped file's sha256+size (walked from `stage/`,
  self-consistent by construction). `--check FILE --stage DIR` is the consistency gate
  (exit 21 on sha-drift / extra / missing file). Both windows-genlock.yml and
  windows-genlock-fast.yml generate + assert it. **The build genuinely rebuilds OBS +
  DistroAV from `vendor/` source — zero checked-in/cached DLLs** (`git ls-files vendor |
  grep .dll` = EMPTY), so #119's stale-prebuilt root cause is structurally gone.
  **GIT-BASH FOOTGUNS (#239) — this script runs under `set -euo pipefail` on the
  windows-2022 runner's git-bash, where the ~2000-file real bundle hits races the 5-file
  Linux unit stages NEVER do (a Windows-only break can pass every PR — full build is
  workflow_dispatch-only, see #240):** (1) **SIGPIPE poisons pipefail** — a per-item
  `printf … | grep -q…`/`… | head -1` lets the downstream early-exit SIGPIPE the upstream,
  and pipefail nondeterministically marks the pipeline failed → ~half a VALID bundle
  falsely "not in manifest" (exit 21 on correct bytes). Fix: single-pass `comm` over
  `LC_ALL=C`-sorted lists (extra=`comm -23`, missing=`comm -13`, both=`comm -12`), and
  `sed '…;q'` instead of `| head -1`. **`comm` MUST run `LC_ALL=C` matching the sort** or it
  sees the lists as unsorted and emits garbage. (2) **proc-sub can truncate** — `done < <(…)`
  FIFO can be cut short on git-bash; materialise the list into a var + iterate with `<<<`,
  and `assert_manifest_complete EXPECTED ACTUAL` (exit **22**, distinct from --check's 21)
  fails LOUD at generation if intended-count ≠ written-count so a partial manifest never
  reaches --check. RED→GREEN must be proven at the SHELL level (Tier-0 blocks the Rust test
  runner); the Windows-specific fix is ONLY verifiable by dispatching `windows-genlock.yml`
  on your HEAD — the broken step never ran green on a real Windows bundle before.
- **per-component BUILD SHA + capability (#122, DONE)** — drift-guard `--compare` now
  CONSUMES `BUNDLE_MANIFEST.json`: supply `manifest=<path>` and it ALSO checks the live
  rig's `obs.dll`/`distroav.dll` Get-FileHash SHA256 vs the manifest's `files[]` entry
  (matched by BASENAME → both the flat fast-dll `obs.dll` and the nested full-bundle
  `bin/64bit/obs.dll` + `obs-plugins/64bit/distroav.dll` resolve) AND the genlock
  CAPABILITY marker text (`genlock_capability=` — the build-unique `render tick ENABLED`
  / `sub-frame jitter reserve` / `timestamp-aligned release` log lines). A STOCK OBS
  32.1.2 (same version, different bytes, emits NO genlock marker) → DRIFT (exit 20) even
  though every version/setting line reads OK — closes the #119 gap the marketing-version
  facet alone could not. Facet is OPT-IN: no `manifest=` → historic version-only contract;
  with it, an unread live SHA/capability is UNKNOWN (exit 11), never a silent clean. New
  pure fns `manifest_sha_for_component` + `genlock_capability_from_log` +
  `drift_check_capability` (tested in `tests/drift_guard.rs`). Driven by `/drift-guard`
  step 1d (Get-FileHash the DLLs read-only + grep the genlock markers + `gh run download`
  the build's manifest). GOTCHA: `genlock_capability_from_log` MUST `return 0` on the
  absent case (empty output IS the "stock" signal) — a bare `[ -n "$line" ] && echo 1`
  returns 1 and trips the test harness's `set -e`. LIVE-PROVEN both boxes 2026-06-25:
  obs.dll `24e22357…` (= #184 fast manifest), distroav.dll `66cea70…` (= full bundle),
  marker present → NO DRIFT; a wrong SHA + no-marker log → exit 20.
- **whole-bundle post-deploy byte/SHA verify (#121, DONE)** — #122 above checks only the
  two genlock DLLs; #121 raises it to deploy-from-clean-tree's contract: drift-guard
  `--compare` now also takes `bundle_hashes=<relpath=sha256,…>` (every deployed bundle
  file's live `Get-FileHash`, gathered off the box) and walks the manifest's WHOLE
  `files[]`, FAILING on ANY mismatch (DRIFT exit 20) or any unread file (UNKNOWN exit 11)
  — so a partial/corrupted deploy where even one NON-DLL file (`obs64.exe`, a first-party
  plugin, a locale) is stale can never pass. New pure fns `manifest_all_paths` +
  `manifest_sha_for_path` + `observed_sha_for` + `drift_check_all_files` (tested in
  `tests/drift_guard.rs`). The facet is OPT-IN and SUPERSEDES the #122 two-DLL SHA checks
  when `bundle_hashes=` is supplied (it already covers obs.dll + distroav.dll by exact
  path); without it the #122 hot-swap obs.dll-only verify is unchanged. The deploy step
  ALSO records a `DEPLOYED_MANIFEST.json` next to the install on each box (the live
  per-file `Get-FileHash` set, same shape as `BUNDLE_MANIFEST.json`) so the deployed bytes
  are auditable on the box after the fact. Driven by `/drift-guard` post-deploy verify
  (gather every file's `Get-FileHash` → `bundle_hashes=`). Both consume
  `BUNDLE_MANIFEST.json`.
- **single canonical OBS plugin-load path (#124)** — OBS scans MULTIPLE module
  locations (`C:\Program Files\obs-studio\obs-plugins\64bit` first-party,
  `C:\ProgramData\obs-studio\plugins\<plugin>\bin\64bit` global,
  `%APPDATA%\obs-studio\plugins\<plugin>\bin\64bit` per-user), so the SAME
  `distroav.dll` in more than one lets a **stale copy silently shadow the intended
  build** (that's #119 in another guise). **CANONICAL = `C:\ProgramData\obs-studio\
  plugins\distroav\bin\64bit\distroav.dll` — exactly ONE copy, there.** Verified live
  on strih + stream 2026-06-25: one `distroav.dll` per box (663040 B), loaded by the
  Program Files genlock `obs64.exe`, render tick ENABLED; **none** under
  `Program Files\obs-studio\obs-plugins\64bit` (the `data\obs-plugins\distroav` folder
  there is resources/locale, not the binary). First-party OBS plugins ship in
  Program Files\obs-plugins; DistroAV is the one in ProgramData — a deploy MUST NOT
  also drop `distroav.dll` into Program Files\obs-plugins (re-creates the shadow). The
  drift-guard now reads `distroav_dll_paths` (every `distroav.dll` location across the
  scan paths, gathered via win-* MCP — `/drift-guard` step 1c) and FAILS if there is
  more than one, or the lone one is off the canonical path. Pin row +
  `drift_check_plugin_paths` (tested in `tests/drift_guard.rs`) live in
  `vendor/README.md` under `canonical_plugin_path`.
- **prod burn guard + `--status` (#246, DONE; burn model updated by #257)** — the measurement burn
  must NEVER be left on in prod. **Pre-#257** it was a launch-env QR burn
  (`OBS_BURN_QR`/`OBS_BURN_QR_PX`/`OBS_BURN_RUN_ID`); RUN 235001 set them in **Machine** scope on
  stream+strih → QR drew on the LIVE broadcast (survives reboot) — the incident this guard exists for.
  **#257 removed all `OBS_BURN_*` env**: the burn is now a per-source `genlock_burn` bool toggled at
  runtime over OBS WebSocket (no env, no restart). drift-guard `--compare` keeps the `burn_env=` key
  for contract stability, but its value is now the **`genlock_burn` WS state** read off each
  program-feeding source (`none` when all off, else a `SOURCE=on` list) — it FAILS (exit 20) on ANY
  source left burning. OPT-IN like the manifest facet (omit the key → dormant, no UNKNOWN → every
  historic `--compare` call unchanged). `drift_check_burn_env` (tested in `tests/drift_guard.rs`).
  Also a read-only `scripts/drift-guard.sh --status host=… genlock_wall_clock=… genlock_capability=…
  burn_env=…` that prints genlock gate + build marker + burn state in ONE place (always exit 0;
  `--compare` is the gate; the rich live OBS dock is the separate #188). Toggling the rig into / out
  of test mode (the burns) is `scripts/rig-mode.sh test|event`, which drives `obs_burn_filter.py
  add|remove` over WS (the #128 wrapper itself is env-free now). The recording-e2e cleanup trap also
  clears+verifies burns off via `obs_burn_filter.py remove`+`check` on both boxes over obs-websocket
  (this cleanup step predates #701, which proved plain OpenSSH+password scp/ssh actually WORKS
  against strih (10.77.9.202) and stream (10.77.9.204) specifically with the `targets.md` creds —
  the WS-based toggle here is kept because it's already the simplest, no-restart mechanism for a
  bool state flip, not because ssh is unreachable; with no burn env any more, the WS `genlock_burn`
  toggle + `rig-mode event` is the whole story).
- **#237 (DONE)** — `manifest_sha_for_component` bracket-escapes the dll-basename dot
  (`obs[.]dll`) so it is matched literally not as a regex wildcard; an obs.dll-only
  manifest labels a supplied distroav SHA `SKIPPED` (not `OK`) — an unchecked value must
  never read as verified (verdict stays NO DRIFT; SKIPPED ≠ DRIFT/UNKNOWN).
- **per-source genlock FIFO held-latency (#357, DONE)** — the FIFO audit line
  `genlock-fifo audit 'SOURCE': … latency_ms=N src_latency_ms=M global_latency_ms=P …`
  has THREE latency fields; drift-guard must parse the EFFECTIVE value `latency_ms=N`
  (the one with a SPACE before it), NOT `src_latency_ms` or `global_latency_ms` (both have
  underscores). Pattern: `sed -n 's/.* latency_ms=\([0-9][0-9]*\).*/\1/p'`.
  OPT-IN `genlock_source_latency=NAME=N,NAME2=M` key in `--compare`; dormant without it
  (historic calls unchanged). Pins are HOST-KEYED: strih = `NDI cam5=3,NDI cam1=3,NDI cam3=3`
  (follows global 3ms floor, structural exact-match). `drift_check_source_latency` (tested in
  `tests/drift_guard.rs`). **RC priority**: DRIFT (rc=2) MUST come before UNKNOWN (rc=3) in the
  return chain — same as all sibling checkers (`drift_check_all_files`, `drift_check_inputs`);
  inverted order silently turns a mixed DRIFT+UNKNOWN case into exit 11 instead of exit 20.
- **stream A/V-align pin is CALIBRATION-TRACKED, not a constant (#390, DONE — supersedes the
  "stream = `NDI 2ME PGM=450`" framing above and the "re-pin ONLY on deliberate rollout" note
  at the bottom of this file).** The A/V-align latency on `NDI 2ME PGM` is whatever the #188
  calibration (`scripts/av_sync_calibrate.py`, #427) last measured and applied — it changes
  EVERY re-calibration, so a hardcoded ms pin goes stale (proven live 2026-07-01: pin said
  `450`, genuinely-delivered live value was `1000` — a false DRIFT). The pin is now
  `NDI 2ME PGM=range:3-2000` (`drift_check_source_latency`'s new `range:MIN-MAX` mode — any
  value inside the DistroAV clamp is OK, `GENLOCK_LATENCY_MS_MIN`/`_MAX` in drift-guard.sh,
  mirrored from `av_sync_calibrate.py`'s `LATENCY_MIN`/`LATENCY_MAX`). A SEPARATE opt-in
  `av_sync_calibrated_ms=<applied_latency_ms>` key (read from `av-sync-last.json` on the OBS
  box's ProgramData, best-effort — drift-guard runs on dev1 and can't reach that path) cross-
  checks the live value against the #427-persisted calibration (±10ms) to still catch a genuine
  hand-nudge drift the range check alone would miss. **Never re-pin the RANGE for a
  re-calibration — only the calibrated ms value changes, which is tracked live, not in the
  manifest.**

GOTCHA: the 150-min `windows-genlock.yml` is `workflow_dispatch`-only (can't run
per-PR), so manifest LOGIC is proven on the Linux `test` job; editing
`windows-genlock-fast.yml` itself triggers the fast Windows build (its `paths:` lists
the workflow file), which then runs the manifest gate on a real built obs.dll.

GOTCHA (workflow source-token gates): BOTH Windows workflows re-assert the genlock patch
tokens in pwsh BEFORE their build (the Linux Rust guards in `tests/genlock_preload.rs` are
probe-gated, can't compile on the runner) — keep them in LOCK-STEP. The slow
`windows-genlock.yml` and the FAST `windows-genlock-fast.yml` must each carry the #136 AND
#245 (`obs_source_set_genlock_latency_ms` / `PROP_GENLOCK_LATENCY_MS_SRC`) gates; the fast
gate was added in #249 (the slow one in #248). `tests/genlock_preload.rs` has a
`windows_genlock[_fast]_workflow_gates_on_the_per_source_latency` guard per workflow — add
the matching guard when you add a new token gate.

GOTCHA (Tier-0 local test verification): the probe-gated test files
(`#![cfg(feature="probe")]`, e.g. `genlock_preload.rs`) AND the whole `src/probe/genlock.rs`
Rust mirror are NOT seen by the default-feature gate (`cargo check/clippy/test --no-run`
compile them to nothing) — so the default gate green proves NOTHING about a genlock C/mirror
change. Grep-level verification of the vendored-source guard strings is the cheap default.
BUT: when a change is a bug-fix needing RED→GREEN proof (regression-test-first — grep alone
can't show a guard test actually FAILS then PASSES), OR to avoid burning the ~150-min
windows-genlock CI on a probe compile/lint/logic error, run the probe-gated genlock tests
locally ONCE via the documented `# airuleset:build-ok` bypass — TARGETED so it's cheap:
`cargo test --features probe --test genlock_preload <name>  # airuleset:build-ok`,
`cargo test --features probe --lib genlock  # airuleset:build-ok`,
`cargo clippy --features probe --all-targets -- -D warnings  # airuleset:build-ok`.
This pulls the probe deps (image/qrcode/rqrr/drm/lz4) → `target/` jumps to ~3.5–4 GB; the
pre-push hook (`scripts/purge-target.sh`) trims it. The C (`obs.dll`) still builds on the
windows-genlock CI only — local can't compile it; eyeball the C diff for correctness.
The NON-probe test files (`drift_guard.rs`, `harness_recording_e2e_paths.rs`) CAN run
fully Tier-0: `cargo test --no-run` to compile, then run the built
`target/debug/deps/<name>-*` binary DIRECTLY (no rebuild, no violation) to prove GREEN.

**Reading genlock state — from the OBS LOG, never an env (no genlock env exists post-#257):**
render tick + ts-align are build defaults, so there is nothing to read in env. The running genlock
state is the OBS log line `genlock: … render tick ENABLED|DISABLED` (latched at OBS launch) +
`genlock: latency = N ms` (once a genlock_fifo input is live). (Pre-#257 a win-* MCP `$env:` read was
additionally a STALE snapshot — the child inherits the long-lived MCP process's env — which is why the
log, not env, was always the source of truth.)

**AHK on strih:** `D:\_APPS\NL_STARTUP.ahk` auto-relaunches obs64 from
`C:\Program Files\obs-studio` (which is the genlock build). On reboot AHK relaunches it and genlock
comes up automatically — it is a build default (#257), no env needed.

Other OBS installs on strih (`D:\_APPS` — 1ME/2ME/vestibul/input/light) — NOT touched;
only the Program Files 2ME is the broadcast one.

## Monorepo Direction (User Directive — zapamätaj si)

1. **strih.lan is the master NTP clock** (DanteSync). Verify clock parity first before any genlock work.
2. Achieving proper OBS genlock is Claude's task (the team's earlier forks never reached a flawless result).
3. **Do NOT use or modify the existing forks** (`~/devel/obs-studio`, `~/devel/DistroAV`) — they are reference/history only (superseded 2026-06-12).
4. **Fresh vendored OBS + DistroAV + NDI SDK** go INSIDE the camera-box repo (ONE common repo). A new NDI SDK version is the basis.
5. Disable the OBS upgrade dialog in the build (prevents stock OBS auto-overwriting the custom version).
6. **Audio sync comes later** — only after zero-loss frames achieved.
7. A future slash command applies new upstream releases into the repo.

## Old Forks (Read-Only Reference)

`~/devel/obs-studio` (branch dev) — adds `get_scheduled_frame` / `async_scheduled` /
`async_wall_clock` to libobs; "Patch os_gettime_ns to apply PTP clock correction from DanteSync".
`~/devel/DistroAV` (branch dev) — runtime-loads `obs_source_set_async_scheduled` via GetProcAddress.
`~/devel/camera-box/distroav-fixed/…/distroav.so` — Linux ELF, NOT deployable to Windows boxes.

These are reference only — the correct direction (scheduled-frame / PTP path and its pitfalls)
is captured here. Do NOT copy or commit changes to them for new work.

## Drift Guard

`scripts/drift-guard.sh` + `/drift-guard` enforces the pinned zero-loss set:
OBS 32.1.2, DistroAV 6.2.1, NDI runtime 6.3.2.0, genlock_wall_clock=1, and the per-box output fps.
`--check-pins` in CI, `--compare` read-only live. Both boxes verified NO DRIFT (2026-06-14).

**output_fps is HOST-KEYED.** The single `output_fps` pin is gone; the manifest pins
`output_fps_strih=30` AND `output_fps_stream=30` — **Topology v2 (#459, EPIC #466, SUPERSEDES the
#11 mixed-60/30 framing this section used to describe):** strih dropped from the 60fps LED-wall
IMAG role to a 30fps cut-to-stream-only box (the 60fps IMAG role moved to the new imag-nb box,
#458/#463); strih→stream is now a plain 30→30 pass-through (the decimation that used to sit on
this hop now happens on strih's OWN camera ingest instead). So `--compare` still REQUIRES `host=`
and resolves `output_fps_${host}` — it **FAILS LOUDLY (exit 1)** on an unknown/empty host (so no
future box silently defaults to the wrong fps). `--check-pins` validates BOTH host pins present.
The OBSERVED `output_fps=<n>` key (read from the live OBS log) is unchanged — only the PINNED side
is host-keyed. `version-integrity-gate.sh` already adds `host=<box>` per `--win-state` box, so it
works unchanged.

**GOTCHA (#459) — re-pinning ANY `vendor/README.md` value breaks every test that reads the REAL
committed file, silently.** `tests/drift_guard.rs`'s `host=strih`/`host=stream` `--compare` fixtures
(14+ call sites, e.g. `compare_clean_when_observed_matches_the_pinned_set`) supply an OBSERVED
`output_fps=N` that must match the REAL manifest's pin — they do NOT use a synthetic `--readme`, so
they silently start reporting DRIFT the moment you change the real pin, with no compile error to
flag it (only a test failure). Same for `tests/harness_recording_e2e_paths.rs` (asserts literal
script text), `tests/version_integrity_gate.rs`'s `STRIH_PINNED`/`STREAM_PINNED` fixtures, and
`tests/harness_genlock_sender_env.rs`. Before re-pinning a manifest value: `grep -n 'host=strih\|
host=stream\|output_fps=' tests/drift_guard.rs` (and grep the OLD value across `tests/`) to find
every affected test FIRST, not after a surprise CI failure.

## #650 — standing :8899 bundle-state service (unattended version-integrity gate)

`scripts/version-integrity-gate.sh --win-state` and `scripts/recording-e2e.sh`'s
`fetch_box_state()` need a live `http://<box>:8899/bundle-state.json` on BOTH strih and stream —
previously this was ONLY ever gathered by hand (`.claude/commands/drift-guard.md` step 1/1b/1c) or
via an ad-hoc `python -m http.server 8899`, so the automatic `pull_request`-triggered
full-path-e2e run always saw both boxes UNKNOWN (exit 11) and refused. Fixed by a STANDING service:

- **Code**: `scripts/bundle_state_gather.py` (pure parsers, unit-tested) +
  `scripts/bundle-state-server.py` (the on-box HTTP flow — regenerates `/bundle-state.json` FRESH
  on every request; serves the record dir, read live via `GetRecordDirectory` over obs-websocket,
  as static files elsewhere). Reuses `obs_phase2.py`'s `_conn`/`_rpc` (auth handshake, #328 stuck-op
  timeout) — do NOT write a fourth OBS-WS client in this repo.
- **`ndi_input_latency` is derived from `genlock_fifo=true`**, not a hardcoded input-name list —
  proven live 2026-07-10 to select exactly the genlocked broadcast-path inputs (camera ingests +
  program feed) on both boxes and nothing else (preview/CG/lyrics inputs never carry
  `genlock_fifo`). If a future scene edit needs a DIFFERENT input excluded from the pin, that input
  must not get `genlock_fifo=true` set — don't hand-maintain a name list here.
- **Deploy**: `C:\ProgramData\camera-box\{bundle-state-server.py,bundle_state_gather.py,
  obs_phase2.py,run-bundle-state-server.ps1,obs-ws-password.txt}` on each box, launched by a
  Scheduled Task `BundleStateServer` (ONSTART trigger, InteractiveToken as `newlevel` — mirrors the
  existing `StartOBS` task). **Both boxes have internet (GitHub) reachability** — deploy/redeploy by
  having the box itself `Invoke-WebRequest` the raw file at a pinned commit SHA
  (`https://raw.githubusercontent.com/zbynekdrlik/camera-box/<sha>/scripts/<file>`) rather than
  transferring file content through an agent's own context (avoids reading a 90KB+ file like
  `obs_phase2.py` into a session just to push it via FileWrite). The OBS-WS password file is the
  ONE thing written directly (FileWrite) — never fetched from GitHub, never committed.
- **GOTCHA — a non-ASCII character in a `.ps1` deployed to these Windows boxes silently corrupts
  parsing.** `run-bundle-state-server.ps1` shipped with an em-dash inside a live double-quoted
  string; Windows PowerShell 5.1 has no BOM on a plain-downloaded file, so it decoded the UTF-8
  em-dash bytes as the system ANSI codepage, breaking the string literal — the wrapper failed at
  launch with a parse error (`At line:39 char:135`) and NEVER started the server, with NO obvious
  error surfaced beyond a generic `LastTaskResult=1`/exit 1 from Task Scheduler. Any `.ps1` deployed
  this way (raw HTTP download, no BOM) must stay pure ASCII — `grep -nP '[^\x00-\x7F]' *.ps1` before
  every deploy. (Python files are NOT affected — Python 3 always assumes UTF-8 source regardless of
  BOM, per PEP 3120.)
- **GOTCHA — `Add-Content` and the `*>>`/`>>` redirection operator default to DIFFERENT encodings
  on Windows PowerShell 5.1** (roughly ASCII/UTF8-no-BOM vs UTF-16LE "Unicode") — mixing them into
  the SAME log file interleaves a NUL byte after every character of whichever stream used the OTHER
  encoding (each ASCII byte silently "widened" to a UTF-16 code unit on readback). Force
  `-Encoding utf8` explicitly on every write path into a shared log file (`Add-Content -Encoding
  utf8`; pipe a subprocess through `| Out-File -Append -Encoding utf8`, never bare `*>>`).
- **Verify**: `curl http://<box>:8899/bundle-state.json` from dev1 (both boxes reachable directly,
  no MCP needed), then feed the two fetched files straight into
  `./scripts/version-integrity-gate.sh --win-state strih=<f> --win-state stream=<f>` locally —
  `GATE PASS` proves the fix without needing a live CI run at all.
- **`GetRecordDirectory` is a real obs-websocket v5 RPC** (`{"recordDirectory": "D:/_REC"}` on
  strih, the `_NLMEDIA stream/RECORDINGS` path on stream) — use it instead of hardcoding/parsing the
  OBS profile `.ini` (multiple stale profile `.ini` files can exist on these boxes; only the LIVE
  RPC reflects which profile is actually active right now).
- **Known gap, filed #651 (not fixed here)**: `rig-busy-gate.sh`'s single-poll
  `obs_phase2.py rig-busy-check` treats one transient `Connection refused` (e.g. OBS restarting
  mid-recording) as `RIG_UNREACHABLE` (exit 43) and aborts the whole 30-min busy-wait immediately,
  discarding correctly-observed "still busy" state from every earlier check. Needs a short
  retry-before-declaring-unreachable inside a single check cycle.

## strih NDI Input → Camera Mapping (1:1, since #753 2026-07-14)

**#753 PIVOT (2026-07-14, binding user directive) — strih's mapping is now 1:1.** The user:
"chcem aby uz bolo ze cam 1 je cam1 ndi source, nie pomenene" (cam N IS the camN NDI source, not
relabeled). `NDI cam<N>` carries `CAM<N> (usb)` for every N=1..7; scene "Cam N" follows the input
1:1 too. Still resolve by the input's `ndi_source_name` when in doubt (never assume from memory)
— but as of this pivot the label and the real camera SHOULD always agree.

| OBS input label | actual NDI src | real camera |
|---|---|---|
| `NDI cam1` | `CAM1 (usb)` | CAM1 (10.77.9.61) |
| `NDI cam2` | `CAM2 (usb)` | CAM2 (10.77.9.62) |
| `NDI cam3` | `CAM3 (usb)` | CAM3 (10.77.9.63) |
| `NDI cam4` | `CAM4 (usb)` | CAM4 (10.77.9.64) |
| `NDI cam5` | `CAM5 (usb)` | CAM5 (10.77.9.65) |
| `NDI cam6` | `CAM6 (usb)` | CAM6 (10.77.9.66) |
| `NDI cam7` | `CAM7 (usb)` | CAM7 (10.77.9.67) |

Each camera's individually-tuned `genlock_latency_ms_src` MOVED WITH the physical camera during
the live rebind (unchanged VALUES: CAM4=20ms, CAM5=8ms, CAM6=13ms, every other camera=3ms — just
re-attached to the input that now actually carries that camera), verified live on strih
2026-07-14.

**HISTORY (pre-2026-07-14, superseded) — strih's mapping used to be INVERTED for the six
original cameras** (cam2 was already 1:1, coincidentally): `NDI cam1`→`CAM3 (usb)`, `NDI cam3`→
`CAM4 (usb)`, `NDI cam5`→`CAM1 (usb)`, `NDI cam4`→`CAM5 (usb)`, `NDI cam6`→`CAM6 (usb)`. Scene
names ("Cam 1"/"Cam 3"/"Cam 5") followed the input labels — same inversion. Kept here for
context only; do NOT resurrect this table.

**GOTCHA for the NEXT full mapping change: grep is not enough — check for a HARDCODED-not-derived
literal too.** Landing the 2026-07-14 pivot required editing far more than the obvious mapping
owner (`set-ndi-mapping.py`'s `DEFAULT_MAP`): `camera-set.sh`'s `camera_strih_route()`,
`recording-e2e.sh`'s `CAMBOX_SWEEP` + its `#286` `BURN_TARGETS` extension list + its `#365`
`FROZEN_CAM_SOURCES` lists, and — the one that would have been EASY to miss — `rig-mode.sh`'s OWN
`STRIH_PROG_SOURCE="${STRIH_PROG_SOURCE:-NDI cam5}"` default. Every OTHER copy of "which strih
input shows cam1" in `recording-e2e.sh` is DYNAMICALLY DERIVED via
`camera_strih_route("$CAMERA_NAME")` (confirmed by a dedicated regression test,
`recording_e2e_strih_scene_and_source_derive_from_the_resolved_camera`, that the string is NEVER
hardcoded there) — but `rig-mode.sh`'s copy (used for its OWN burn-toggle target, `#246`) is a
SEPARATE hardcoded literal with no such test, so it silently kept pointing at the OLD input after
the pivot until caught by manual inspection, not by any test failure. **Before declaring a mapping
change complete: grep every script that has EVER read `camera_strih_route`/`CAMERA_STRIH_SCENE`/
`CAMERA_STRIH_SOURCE` output, and separately grep for the literal `NDI cam<N>`/`Cam <N>` strings
themselves (`grep -rn '"NDI cam[0-9]"' scripts/`) — a hardcoded literal in a DIFFERENT script that
happens to serve the same conceptual role will not show up by tracing one script's own call
graph.** Also worth checking after any future mapping change: `vendor/README.md`'s drift-guard
per-input latency pin tables (`genlock_source_latency_strih`, `ndi_input_latency`) — those encode
LIVE state keyed by input name and need a fresh live re-baseline, never a blind text edit (see
#757).

To enable genlock on a camera's strih ingest: `SetInputSettings genlock_fifo=true`
on the input whose `ndi_source_name` matches that camera (`overlay=true` so other settings persist).

OBS only renders an NDI source when it's on an active scene — `GetSourceScreenshot`
fails (702) on an off-program source; that is not an error.

## OBS NDI-Output Timecode Lag (Root Cause)

OBS NDI-output `timecode` lags real emit ~150 ms. Root-caused 2026-06-15.

**Cause:** DistroAV Main Output (`vendor/distroav/src/ndi-output.cpp:372`) stamps
`NDIlib_send_timecode_synthesize` (sentinel INT64_MAX) and drops OBS's own
`frame->timestamp`. The NDI SDK's `synthesize` seeds a counter from system time ONCE at
stream start (T0) then emits `T0 + N×(1/fps)`. The lag = pipeline buffering frozen into
the seed. `clock_video=false` (:230-231) so SDK doesn't pace.

**Why option B (p_metadata) is impossible:** `struct obs_source_frame` has NO metadata
field → p_metadata dropped at ingest; the output re-creates a fresh NDI frame → NULL.
A per-frame emit-stamp in p_metadata is structurally dropped twice across one OBS hop.

**The fix (B′):** patch DistroAV fork to stamp the real DanteSync wall-clock boundary
instead of `synthesize` — mirror what camera-box already does in `src/ndi.rs:792-805`.
The genlock wall-clock infra exists in the OBS fork. ~10-line helper + change :372 and :423.
Tracked: #76.

The lag CANCELS OBS↔OBS (strih→stream measured correctly = 187 ms). It BREAKS cam→OBS
(cam→strih timecode gave nonsense 17.7 ms / negative). Fix B′ unlocks exact cam→strih
measurement.

## CI exact-anchor guards live in YAML too (not just tests) — #269 gotcha
A vendored-C refactor that changes a pinned genlock call-site SHAPE breaks the
`regex::Escape` `-notmatch` source guards in BOTH places — update ALL in the same change:
- `tests/genlock_preload.rs` (the Rust vendored-source guards), AND
- `.github/workflows/windows-genlock-fast.yml` + `windows-genlock.yml` (the "Assert genlock
  timestamp-aligned release patch present (#136)" step — these run on the whitespace-collapsed
  `$src` BEFORE the ~18-min build, so a stale anchor fails the build at the assert, not at compile).
Example: #148 hoisted `genlock_present_ts(genlock_wall_now_ns(), …)` → `wall_now = genlock_wall_now_ns();`
+ `genlock_present_ts(wall_now, …)`; the YAML guards pinned the old inline form and failed the FAST
build until updated to check the `wall_now` source + the new call form.

## #276 — multiview render-divisor decouple (monitoring must not steal the 60fps program budget)
The built-in OBS Multiview projector renders a thumbnail of every scene each frame (9-18ms) on the
SAME graphics thread that presents the program output, in `obs_graphics_thread_loop` AFTER
`output_frames()`. At 60fps that overruns the 16.6ms budget and breaks the program render whenever
the multiview is OPEN (live A/B on strih: program 4.9ms/0-skip closed → 13.7-14.2ms open). The
lightweight DistroAV "Multiview" NDI-output feed (~1.2ms) is the production monitoring path; the
built-in projector is the budget thief.

**Fix (vendored libobs + frontend):** a PER-DISPLAY render-rate divisor.
- `obs-internal.h` struct obs_display: `uint32_t frame_counter; uint32_t render_divisor;` —
  **PER-INSTANCE, NEVER static** (a static counter would lockstep every projector in the
  `QList<OBSProjector*>`). Read+written only on the graphics thread; the divisor is set once from the
  Qt thread at create (same unguarded pattern as `background_color`).
- `obs-display.c render_display()`: skip BEFORE `render_display_begin()` —
  `if (display->render_divisor > 1 && (display->frame_counter++ % display->render_divisor) != 0) return;`
  Skipping before begin() = ~0 cost AND leaves the last presented frame on screen → NO flicker (the
  rejected callback-skip alternative still paid the clear + `gs_present` and flickered).
- `obs.h` + `obs-display.c`: `EXPORT void obs_display_set_render_divisor(...)`.
- `OBSProjector.cpp` addDrawCallback: `if (isMultiview) obs_display_set_render_divisor(GetDisplay(), 2)`
  — MULTIVIEW display ONLY; program output + preview keep divisor 0/1 (every frame, genuinely
  unaffected).
- **Needs the FULL ~150-min `windows-genlock.yml` build** (it builds the frontend where
  OBSProjector.cpp lives); the FAST libobs-only build compiles the obs-display/internal/obs.h parts
  but NOT the frontend (still source-text-gates the OBSProjector token).
- Pure mirror `display_render_skip(frame_counter, render_divisor)` in `src/probe/genlock.rs` (the only
  unit-testable part — the GPU render timing needs the rig) + cadence tests. Guards live LOCK-STEP in
  `tests/genlock_preload.rs` AND BOTH `windows-genlock{,-fast}.yml` (the #269 rule above): the
  render_display gate token, the API, the struct fields, the OBSProjector `divisor=2` call.
- **DEPLOY GOTCHA — OBSProjector.cpp compiles into `obs64.exe`, NOT a frontend DLL.** `frontend/cmake/
  ui-widgets.cmake` puts OBSProjector in the `obs-studio` target whose `OUTPUT_NAME=obs64`
  (`frontend/CMakeLists.txt`). So the #276 frontend change ONLY takes effect if you swap **obs64.exe**
  (`bin\64bit\obs64.exe`) — the usual obs.dll-only hot-swap is INSUFFICIENT. A #276/#275a deploy swaps
  THREE binaries: `bin\64bit\obs.dll` (libobs) + `bin\64bit\obs64.exe` (frontend) + the canonical
  `ProgramData\…\distroav\bin\64bit\distroav.dll` (#275a). `obs-frontend-api.dll` does NOT need swapping
  (it doesn't use the new export). Use the FULL `obs-genlock-windows-x64` artifact + its
  `BUNDLE_MANIFEST.json` files[] sha256 to byte-verify each swapped binary.
- **RIG-CONFIRMED 2026-06-27 (build 829ec4bb, strih, program on a live 60fps cam, A/B over 60s GetStats
  deltas via WS):** multiview projector CLOSED = 4.34ms / 0 renderSkip / 0 outputSkip / 60.000fps;
  multiview projector OPEN fullscreen (divisor=2) = **11.50ms / 0 renderSkip / 0 outputSkip / 60.000fps**
  (< 16.6ms budget, < pre-fix 13.7-14.2ms). genlock-fifo underruns UNCHANGED by the multiview (~2/s
  steady-state both states; holds flat, overruns=0). Multiview renders LIVE at ~30fps (divisor-2) →
  monitoring works. **#276 PASS — divisor=2 keeps the 60fps program clean with the multiview open.**
  SEPARATE finding #278: **Studio Mode** preview is a 2nd render-budget consumer at 60fps (studio-ON ≈
  14% renderSkip, studio-OFF clean) — prod runs studio off, so latent not active; same class as the
  multiview.

## #278 — multiview ADAPTIVE budget-based decouple (SUPERSEDES the #276 fixed divisor)
The #276 fixed `render_divisor=2` (multiview every-other-frame) is INSUFFICIENT for the 4-LIVE-CAM
case: rig-measured a SINGLE multiview render is ~18-23ms, which ALONE exceeds the 16.6ms 60fps budget,
so even every other frame the rendered frames overran the deadline → **~29% program renderSkip → the
LED-wall IMAG program dropped to ~43fps**. #278 replaces the fixed cadence with an ADAPTIVE,
budget-based skip so the PROGRAM render is NEVER delayed regardless of multiview weight.

**The new model — `render_divisor>1` is now just a "throttleable monitoring display" MARKER** (the
value, still 2, set by OBSProjector on the multiview only — NO frontend change for #278); the actual
skip is driven by the display's measured render cost vs the remaining frame budget:
- `obs-internal.h`: `struct obs_display` drops `frame_counter`, adds **`uint64_t render_ewma_ns`**
  (per-instance EWMA of the actual draw, α=1/4; 0 = cold/not-warmed). `struct obs_core_video` gains
  **`uint64_t graphics_frame_start_ns`** (this tick's `os_gettime_ns()` start).
- `obs-video.c` `obs_graphics_thread_loop`: publishes `obs->video.graphics_frame_start_ns = frame_start;`
  at the TOP of the tick (before output_frames + render_displays).
- `obs-display.c` `render_display()`: for a monitoring display, BEFORE `render_display_begin()` compute
  `elapsed = now - tick_start` and **skip when `elapsed + ewma > budget` where `budget = interval -
  interval/10` (90% margin)**. `ewma==0` / no timing → render once to measure (never starved to 0).
  After a real render, update `render_ewma_ns = prev ? (prev*3 + dur)/4 : dur`. Program + preview
  (divisor 0/1) are NEVER throttled. Skipping before begin() = ~0 cost, last frame stays → no flicker.
- **#278 froze the multiview solid (→ #293).** When a single monitoring render genuinely exceeds the
  whole budget (4-live-cam multiview ~18-23ms > ~15ms), it had NO slack ever → skipped every tick →
  FROZE for a whole live event. #278 called that "fine, it's monitoring" — it is NOT (a camera
  operator needs the multiview live). **#293 added an anti-starvation floor** (see the #293 section
  below): an over-budget display is skipped at most K=3 ticks in a row, then FORCED to render. Re-warm
  after load drops still works via close+reopen (ewma=0), but the floor means it never freezes.
- **DEPLOY is LIBOBS-ONLY** (unlike #276, which changed OBSProjector.cpp → needed the **obs64.exe**
  frontend swap). #278 touches only `obs-display.c`/`obs-video.c`/`obs-internal.h` → the **fast
  obs.dll hot-swap is sufficient** (the `windows-genlock-fast.yml` build validates it). The
  OBSProjector `divisor=2` marker is already deployed; no obs64.exe/distroav swap for #278.
- Mirror `display_render_skip_budget(render_divisor, elapsed, ewma, interval)` +
  `display_render_ewma_update(prev, dur)` in `src/probe/genlock.rs` (replaced the old
  `display_render_skip` cadence mirror) + behavior tests. The #276 every-Nth tokens are GONE.
- **NEW pinned source anchors (the #269 lock-step — in `tests/genlock_preload.rs` AND BOTH
  `windows-genlock{,-fast}.yml` pwsh gates):** `if (elapsed + ewma > budget) return;`,
  `const uint64_t budget = interval - interval / 10;`,
  `display->render_ewma_ns = prev ? (prev * 3 + dur) / 4 : dur;`,
  `obs->video.graphics_frame_start_ns = frame_start;`, struct fields `uint64_t render_ewma_ns;` +
  `uint64_t graphics_frame_start_ns;`. (The OBSProjector `if (isMultiview) …divisor…2)` anchor is
  UNCHANGED — the marker stays.) The OLD `(display->frame_counter++ % display->render_divisor)` token
  was removed everywhere.
- **ACCEPTANCE (the supervisor rig-verifies — the prior #276 fix failed exactly this):** multiview
  open showing 4 LIVE cams + program single-cam → program MUST be 0 renderSkip / activeFps 60 /
  avgRenderMs-for-program under budget, while the multiview renders at whatever reduced fps.

## #293 — multiview anti-starvation FLOOR (the #278 budget-skip froze the strih Multiview)
#278's budget skip had NO liveness floor: a 4-cam multiview render (~18-23ms) alone exceeds the ~15ms
budget on EVERY tick → skipped forever → the strih Multiview FROZE solid for a whole live event.
- **The skip decision is now a pure, OBS-dep-free helper** `obs_display_should_skip(render_divisor,
  ewma, elapsed, budget, consecutive_skips)` in **`vendor/obs-studio/libobs/obs-display-budget.h`**
  (`#define OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS 3u`). Over budget → skip ONLY while
  `consecutive_skips < K`; the (K+1)-th over-budget tick is FORCED to render → ~15fps multiview at K=3
  instead of 0fps. divisor<=1 and ewma==0 never skip (unchanged).
- `obs-display.c` `render_display()`: `#include "obs-display-budget.h"`, call the helper,
  `display->render_consecutive_skips++` on skip, reset `= 0` after every real render. New per-display
  `uint32_t render_consecutive_skips` in obs-internal.h. LIBOBS-ONLY (obs.dll hot-swap, like #278).
- **Accepted tradeoff:** the forced render every K+1 ticks costs ~1 program frame during sustained
  4-cam overload — the user-chosen price of never-freeze (vs #278's frozen-but-0%-hit). Do NOT
  "fix" this by removing the floor; #293 is exactly the requirement that the multiview stay live.
- **TESTABILITY PATTERN (reusable for ANY vendored-C decision):** extract the decision into a pure
  header (only `<stdbool.h>`/`<stdint.h>`, a `static inline`) and unit-test it from a DEFAULT-features
  Rust integration test that compiles+runs a tiny `cc -std=c11` harness over the real header
  (`tests/obs_display_budget.rs`). This sidesteps BOTH the probe-local-build ban AND "Linux can't
  build libobs" — the test exercises the EXACT production code, RED→GREEN verifiable locally, no probe.
- **Pinned source anchors changed (the #269 lock-step — update ALL of `tests/genlock_preload.rs`
  (probe-gated) + BOTH `windows-genlock{,-fast}.yml` pwsh gates together):** the OLD
  `if (elapsed + ewma > budget) return;` anchor is REPLACED by
  `if (obs_display_should_skip(display->render_divisor, ewma, elapsed, budget,` +
  `display->render_consecutive_skips++;` (obs-display.c) and a new `$bud` read of obs-display-budget.h
  asserting `return consecutive_skips < OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS;`. Struct anchor added:
  `uint32_t render_consecutive_skips;`. The `budget`/EWMA/`graphics_frame_start_ns`/divisor anchors stay.
- The Rust `display_render_skip_budget` mirror in `src/probe/genlock.rs` stays the BUDGET-only predicate
  (the floor is layered by the C caller); its doc says so. The MSVC compile of the header rides
  `windows-genlock-fast.yml` (fires on `vendor/obs-studio/**` push). Live 4-cam unfreeze = supervisor
  rig step at the coordinated OBS rollout.

## #758 — Studio Mode render cost (HISTORY — the "imag must run it OFF" mandate is SUPERSEDED)

**⚠️ SUPERSEDED (user, 2026-07-18): "vypínať studio mód je dávno prekonaný stav"** — the OFF mandate
below dates from the era when imag still had other render-budget problems (e.g. everything pinned to
one core). Current state: imag_scenes.py --bootstrap deliberately sets **Studio Mode ON** (the
operator cut workflow needs it) and the render budget holds fine (live 2026-07-18: 60.0 fps /
6.3 ms / 0.03 % skips WITH Studio ON). Do NOT turn Studio Mode off on imag as an "optimization" —
the A/B numbers below are kept only as historical context of what Studio Mode cost back then.

### (HISTORY) original #758 finding — Studio Mode as a render-budget consumer
A stale **Studio-Mode-ON** left over from an earlier debugging/testing session on imag degraded its
render loop right to the edge of (and past) a healthy budget — confirmed live 2026-07-14 by direct
A/B `GetStats` measurement on the SAME box, same Multiview-open state: **Studio ON ≈ 56.8-58.1fps /
15.5-17.1ms render / renderSkip climbing to ~4%; Studio OFF ≈ 60.00fps / 7.9-10.3ms / 0% renderSkip,
stable across 5+ repeated windows.** This is a DIFFERENT consumer from the Multiview projector
itself (#276/#278/#293 above) — it stacks on top of it, and unlike the Multiview it has no
documented reason to be on for imag's operator workflow (imag has no operator staging a NEXT scene
via a Preview pane the way a vision-mixer operator would; MV+Program projectors are the actual
consumed outputs). **Fix, now automated:** `scripts/obs_phase2.py ensure-studio-mode-off` (idempotent
— reads `GetStudioModeEnabled`, only calls `SetStudioModeEnabled: false` when it's actually ON) is
wired into `recording-e2e.sh`'s `[0/8]` preflight, right after the MV/Program projector-open step and
BEFORE render-health is measured. **imag-ONLY** — do NOT point this at strih/stream; their Studio
Mode stays ALWAYS ON per a separate, unrelated, hard user directive for those two boxes (see the
`Studio Mode always ON both boxes` memory note — that note is scoped to strih+stream, it does NOT
mean imag too). If you ever manually toggle Studio Mode on imag while debugging something else,
remember to run this (or just re-run the `[0/8]` preflight) before trusting a render-health reading.

## #758 — MV render-divisor CAPABILITY check has NO OBS-WS signal; reuses the setup-imag.sh nm check
`render_divisor`/the EWMA budget-skip state (#276/#278/#293) has **no obs-websocket `GetStats` field**
— there is no RPC that reports "is this display's monitoring-throttle path compiled in / actively
engaging". The only real, checkable evidence is STATIC: the exported `obs_display_set_render_divisor`
symbol in the deployed frontend binary (`nm -D -u /usr/bin/obs | grep obs_display_set_render_divisor`)
— the SAME check `scripts/setup-imag.sh` already runs at provisioning time (#499). `recording-e2e.sh`'s
`[0/8]` preflight now ALSO runs this (read-only, over SSH) on every ALL_CAMBOX run, not just at
provision time — WARN-only for now (`IMAG_DIVISOR_CAPABILITY_FAIL` defaults to `0`; flipping to FAIL
is a one-line change once the Linux frontend genuinely carries the symbol on a build that also
proves it ENGAGES — that engagement proof is separate future #756 work, this check only proves the
symbol is LINKED). Live-checked 2026-07-14: the symbol WAS present on that day's build — if you see
this WARN fire, it means a regression (a frontend swap that dropped the symbol), not the pre-existing
"known gap" some earlier dispatch text assumed was still true; always re-check live, don't trust a
stale "known gap" framing (`verify-issue-still-valid.md`'s general lesson, applied here specifically).

## #758 — cam1's post-deploy NDI reconnect genuinely takes ~11-20s; don't under-budget a reverify
`preflight_mv_reverify` (the sender-bounce liveness re-check after a camera's `[2/8]`/`[2b/8]`
service→burn-unit swap) originally budgeted ~13s total (one ~3.5s check, one re-attach, a fixed 2s
settle, one more ~3.5s check) before declaring the leg dead. TWO real CI acceptance runs failed cam1
on this exact mechanism. A direct timed `systemctl restart camera-box` on cam1 + repeated
`frozen-camera-gate.py` polling against `MV NDI cam1` measured genuine recovery at **t+11.4s** — a
real, physical NDI reconnect latency (v4l2 device re-open + USB renegotiation + NDI announce +
strih's receiver re-discovery), not a dead camera. Widened to a 3-attempt × 6s-settle retry loop
(mirrors this file's OWN `FROZEN_CAM_ATTEMPTS`/`FROZEN_CAM_RETRY_SLEEP` precedent for the mid-run
frozen-camera-gate, just sized smaller since this fires per-camera up to 7x per sweep). **If you add
a NEW post-deploy liveness/reverify check anywhere in this harness, measure the real reconnect
latency live FIRST** (a plain restart + poll loop, no code needed) rather than guessing a budget —
a guessed-tight budget reads as "the camera is dead" when it's actually just still reconnecting.

## #275 — cheaper measurement burn (bulk-fill render); wire format is LOCKED by the #186 fixtures
The per-frame QR burn (strih/stream `genlock_burn` filter `vendor/distroav/src/burn-qr.hpp` render())
was a per-pixel `put_bgra` loop with per-pixel bounds checks → 18.9ms on a 60fps strih render (>16.6ms
budget). Fix = BULK fills: clamp the QR square to the frame ONCE, white backing = one `memset(row,
0xFF, …)` per row (BGRA white is all-0xFF), black module runs = a tight 32-bit run-fill (portable
black via `memcpy({0,0,0,255})`). **Output bytes are IDENTICAL** → the rqrr decoder + the #186
fixtures are unaffected (the `burn_payload_parity` render→decode test is the proof). Guard:
`burn_qr_render_uses_bulk_fills` + the YAML burn gate (lock-step).

**GOTCHA — you CANNOT shrink the burn QR matrix to a smaller version here.** The matrix size = f(payload
length, ECC). ECC must stay HIGH (NDI recompression). Shrinking the payload changes the **wire format**
`P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}`, which is pinned by the #186 regression lock:
`tests/fixtures/burn-unreadable/*.png` are REAL rig recordings carrying that exact format and
`burn_fixture_decode` asserts `Payload::decode` reads them. A format change breaks those fixtures, which
can only be regenerated by a fresh rig recording run — so a payload-shrink is **not shippable with green
CI off-rig**. The memset win is the off-rig-shippable lever; a matrix-shrink needs rig fixture regen.

## #147 — backward wall-clock step recovery (SINK ts-align)
The SINK ts-align release (`obs-source.c ready_async_frame`, the `due==0` branch) used to HOLD
(repeat the last frame) forever on a BACKWARD DanteSync wall-clock step (NTP/PTP sawtooth):
`present_ts = wall_now - reserve` regresses below every queued (pre-step, high) frame ts → due==0
every tick → since #257 genlock is always-on this FREEZES the live program feed until the clock
climbs back. This is the SINK twin of the cam-EMIT freeze the #131/#134 `genlock_emit_gate` guard
fixed (a boundary impossibly far in the future re-latches).

- **Fix mechanism = FUTURE-stamped detection, NOT a stored `last_wall_now`.** A queued frame
  stamped `> wall_now + interval` is impossible for a live capture (= captured before the step). On
  due==0 with such a frame, RE-ANCHOR instead of freezing.
  Why not a one-shot "clock just stepped" detector: the stale future-stamped frame PERSISTS across
  the following ticks (until it drains), so a one-shot trigger re-freezes next tick. Future-stamped
  self-heals over the non-monotonic seam (stale-high then fresh-low frames) with NO per-source state.
  Pure mirror: `src/probe/genlock.rs genlock_release_guarded` / `GenlockReleaseGuarded`.
- **#269 deep-review fixes (the three things the original re-anchor got wrong):**
  - **[0] DON'T drain to empty.** The old re-anchor presented the NEWEST and dropped `num-1` →
    collapsed the buffer; for a deep per-source latency (≤2000 ms) the feed then FROZE for
    ~latency_ms while it refilled. Fix: present the OLDEST queued frame, drop NOTHING extra
    (`to_drop = 0`); `get_closest_frame` erases the presented head, so the buffer drains
    one-frame-per-tick (consume rate) and the latency depth is PRESERVED — a few-frame blip at ANY
    latency.
  - **[3] trigger on the MAX queued ts, NOT `array[0]` (the oldest).** The oldest-frame trigger is
    queue-DEPTH-dependent: a step smaller than a deep source's buffer left its oldest frame
    not-future → it stayed frozen while a shallow source jumped → cross-source DESYNC. The MAX is
    depth-independent → all sources re-anchor UNIFORMLY once the step exceeds one interval.
    `async_frames` is ARRIVAL order (non-monotonic across the seam) → scan for the true max.
  - **[2] count + log ONCE per EVENT, not per tick.** A step recovers over ~depth ticks; the old
    `genlock_backward_steps++`-every-tick counted one event as N and logged at frame rate (broke the
    5 s audit gating). Latched via the `bool source->genlock_in_backward_step` field (set true on the
    rising edge, cleared on a benign/normal tick). Mirror: `genlock_backward_step_latch` /
    `GenlockBackwardStepLatch`.
- **A large per-source latency (up to 2000 ms) is SAFE** — it buffers PAST-stamped frames (aging,
  `ts <= wall_now`), whose max is never `> wall_now + interval`, so the guard never triggers on it.
- **Audit signal:** the `genlock-fifo audit` line prints `backward_steps=%llu`
  (`source->genlock_backward_steps`) — read it on the rig deploy-verify to confirm recoveries (now
  one increment per real step event, not per tick).
- **GOTCHA — the `ts_align_hold_counts_as_hold_not_underrun` guard pins `genlock_holds++` within a
  HARDCODED window** of the `genlock_present_ts_reserve(wall_now, reserve_ms)` anchor
  (`tests/genlock_preload.rs`). Adding comment/code in the `due==0` block can push `genlock_holds++`
  out of that window and fail the guard — #269's max-ts scan + comments grew the block, so the window
  was widened 2500→4000 chars. Keep that block reasonable; bump the window if you add more.
- **Pinned source tokens** (in `tests/genlock_preload.rs` + BOTH windows-genlock*.yml per the #269
  lock-step): `max_ts > wall_now + interval`, `source->genlock_backward_steps++`,
  `source->genlock_in_backward_step = true`/`= false`. (The pre-#269 token was
  `source->async_frames.array[0]->timestamp > wall_now + interval` — replaced by the max-ts trigger.)

## Per-source latency WebSocket API + delivery gate (#358)

**OBS WebSocket key for per-source genlock latency:** `genlock_latency_ms_src`
(from `PROP_GENLOCK_LATENCY_MS_SRC` in `vendor/distroav/src/ndi-source.cpp:38`).
Use with `GetInputSettings` / `SetInputSettings` (`overlay=true`).

**Render-Delay filter kind:** `gpu_delay`
(from `.id = "gpu_delay"` in `vendor/obs-studio/plugins/obs-filters/gpu-delay.c:348`).
Use with `GetSourceFilterList` / `SetSourceFilterEnabled`.

**CI delivery gate — `_latency_delivery_ok(set_ms, delivered_ms, tolerance_ms=100):`**
In `scripts/obs_phase2.py`. Returns `delivered_ms >= set_ms - tolerance_ms`.
The #292 bug force-drained the FIFO to ~3-50ms even at 1000ms setting → threshold 1000-100=900ms
clearly catches it. This pure function is Tier-0 testable (no OBS calls).
CI proves the code; the SUPERVISOR runs the live rig verify (set 1000ms on prod `NDI 2ME PGM`,
measure actual delivery, restore the LAST-CALIBRATED A/V-align value — post-#390 this is whatever
`av-sync-last.json`'s `applied_latency_ms` says, NOT a fixed constant; re-run
`scripts/av_sync_calibrate.py --apply` if the calibrated value itself is unknown/stale).

**Snapshot+restore pattern (mirrors `_TEST_PRELOAD_STATE_KEY`):**
`_TEST_LATENCY_STATE_KEY = "test_latency_saved"` in `scripts/obs_phase2.py`.
Save BEFORE changing (crash-safe), restore in `teardown()` BEFORE `_restore_test_preload`
(prod A/V-align back first), read-back verify with LOUD WARN on mismatch (#246 burn-verify pattern).
CLI env: `GENLOCK_TEST_LATENCY_SOURCE` / `GENLOCK_TEST_LATENCY_MS` (default 1000);
wired into `recording-e2e.sh`.

**prod A/V-align on stream (#390: value is CALIBRATION-TRACKED, not a fixed 450 any more):**
`NDI 2ME PGM` runs whatever `scripts/av_sync_calibrate.py` last applied — restore that value
(read `av-sync-last.json`'s `applied_latency_ms`, or re-calibrate) after any test window, NOT a
hardcoded number. The drift-guard MANIFEST pin (`vendor/README.md`) is
`genlock_source_latency_stream = NDI 2ME PGM=range:3-2000` — re-pin the RANGE only if the
DistroAV clamp itself ever changes; NEVER re-pin it to a specific ms value for a re-calibration
(that is the exact stale-constant bug #390 fixed).

**Audit line disambiguation (see also #357):** `latency_ms=N` (space before) = effective held
value; `src_latency_ms=M` (underscore prefix) = per-source setting. The regex
`r"genlock-fifo audit '([^']+)'.*? latency_ms=(\d+)"` (space before `latency_ms`) captures the
effective, not the setting.

## #484 — genlock render-tick thread pinned SCHED_FIFO to the isolated core (imag-nb, Linux-only)
The libobs graphics thread (`obs_graphics_thread`, `vendor/obs-studio/libobs/obs-video.c`) drives
the wall-clock-slaved genlock render tick (`video_sleep` → `genlock_next_deadline`). On imag-nb it
is now pinned by `genlock_pin_render_tick_thread()` — called from `obs_graphics_thread` right after
`os_set_thread_name`, `#if defined(__linux__) && !defined(_WIN32)` guarded (Windows/macOS builds are
unaffected; imag-nb is the only Linux OBS box):
- **Affinity** → the kernel-reserved cores, DERIVED from `/sys/devices/system/cpu/nohz_full`
  (robust, like `src/affinity.rs`), hardcoded `{10,11}` fallback tied to #483's `nohz_full=10,11`
  reservation, via `pthread_setaffinity_np(pthread_self(), ...)`.
- **Scheduler** → `sched_setscheduler(0, SCHED_FIFO, &param)` with a **LOW** prio (`GENLOCK_RT_PRIORITY
  10`). `pid 0` = the calling THREAD on Linux, so ONLY the render-tick thread goes FIFO (NOT the
  ~106-thread process — that would hang the box).
- **WARN-and-CONTINUE**: any failure logs `genlock: … — continuing SCHED_OTHER (#484)` at
  `LOG_WARNING` and runs on. NEVER abort/retry/hang (a high-prio runaway FIFO thread hangs a headless
  box). Mirror of `src/affinity.rs` (#289).
- **rtprio grant dependency**: OBS runs as the unprivileged desktop user, so the SCHED_FIFO call
  needs `setup-imag.sh`'s `/etc/security/limits.d/95-imag-genlock-rtprio.conf` (`${DESKTOP_USER} -
  rtprio 20`, applied by PAM at next login). Without it the pin warn-degrades to SCHED_OTHER.
- Guards: `tests/genlock_rt_pin.rs` (Tier-0 vendored-source, RED→GREEN locally) +
  `tests/setup_imag_guards.rs` (rtprio drop-in). The C build proof is `linux-genlock.yml` SUCCESS.
- **Live cyclictest/chrt verification (rig redeploy + before/after jitter, with rollback) is the
  SUPERVISOR's post-merge step** — never rig-verified from the worker (a misbehaving SCHED_FIFO
  thread can hang the headless box).

**GOTCHA — `_GNU_SOURCE` in a vendored-OBS Linux patch that uses GNU syscalls.** `cpu_set_t` /
`CPU_SET` / `pthread_setaffinity_np` / `CPU_COUNT` are GNU extensions gated on `_GNU_SOURCE`, which
MUST be defined BEFORE the first libc header (`<time.h>` pulls `<features.h>`). Add
`#ifndef _GNU_SOURCE / #define _GNU_SOURCE / #endif` at the TOP of the `.c` (the `#ifndef` guard is a
no-op if the build already sets it globally — OBS does on Linux, but self-contained is bulletproof),
then the Linux-guarded `#include <pthread.h> <sched.h> <errno.h>`. `sched_setscheduler`/`SCHED_FIFO`
are POSIX (no `_GNU_SOURCE` needed) but the affinity side is GNU. De-risk the ~8-30 min
`linux-genlock.yml` build FIRST by compiling the added helpers standalone: `cc -std=c11 -Wall -Wextra
-D_GNU_SOURCE helpers.c -lpthread` with `blog`/`LOG_*` stubbed (caught 0 issues here, but the
pattern turns a 30-min CI miss into a 2-second local one).

## #286 — emitted genlock timecode must key on CAPTURE instant, not ARRIVAL (root A/V-cut cause)

The camera-box appliance used to stamp its emitted NDI genlock timecode from `wall_clock_ns()` read
at NDI-SEND time (arrival), not from when the V4L2 buffer was actually captured. Each grabber
card's own photon→dequeue latency (`d_X`, real and per-card — #624 measured cam1/cam3 ~70ms vs
cam4 ~56ms, a 15.78ms spread) then leaks straight into the stamp. A genlock receiver that aligns
FIFO release on stamp-time cannot equalize that real hardware skew — cutting between cameras
visibly shifts perceived video timing (the root cause behind repeated A/V-cut complaints).

**Fix (`src/genlock_stamp.rs`, pure Tier-0):** `capture_realtime_100ns(capture_monotonic_100ns,
mono_to_real_offset_100ns)` converts the V4L2 buffer's OWN kernel `CLOCK_MONOTONIC` timestamp
(`metadata.timestamp`, assumed `TIMESTAMP_MONOTONIC` per the V4L2 default — not runtime-verified
against `metadata.flags`, a known non-blocking gap) to wall-clock via a periodically-resampled
mono→real offset, then `genlock_emit_timecode_100ns` keys the emitted boundary decision on THAT
value, discarding arrival time entirely (kept only as an unused diagnostic argument for a future
`stamp_arrival_divergence_100ns` wire-up — not yet called from production code, see its doc
comment).

**Critical distinction — #286 fixes the STAMP, not the hardware latency itself.** The receiver's
genlock FIFO reserve (`genlock_latency_ms_src`, the DistroAV per-source "Latency (ms)" setting,
floor 3ms) must ALSO be raised to ≥ the measured cross-camera spread for the corrected timecode to
actually get USED to equalize cameras — the FIFO can only hold the faster camera's frame long
enough to wait for the slower one if its reserve window is wide enough. Fixing the stamp without
raising the receiver reserve changes nothing observable.

**Proving cross-camera alignment visually:** a SINGLE screenshot of strih's Multiview projector
window (NOT sequential per-source `GetSourceScreenshot` calls — those sample different real-world
instants and are invalid) captures every camera tile's rendered content at the SAME instant. Focus
+ resize the "Projector - Multiview" window via the win-* MCP `FocusWindow`/`App(resize)` first (a
`SetForegroundWindow`-blocked focus attempt is fixed by `MinimizeAll` then retrying `FocusWindow`)
— the projector defaults to a tiny (~742×461) floating window whose tiles are too small to read a
QR tick at native size otherwise.

## TECHNIQUE — find a wall-clock window inside imag-nb's multi-day OBS log via the embedded `ts_present` epoch-ns

imag-nb's OBS log is a SINGLE file that can span many days (it only rotates on process restart,
and imag's own OBS is rarely restarted); each line prints only `HH:MM:SS.mmm` — NO date — so
grepping for a specific day/time window by timestamp alone is unreliable (you'd have to walk
midnight rollovers). Every `genlock-fifo audit '<src>'` line already carries a real UTC epoch-ns
field, `ts_present=<epoch_ns>` — use it directly instead:

```bash
# Convert a target UTC window to epoch seconds:
date -u -d "2026-07-11T04:09:00Z" +%s   # -> 1783742940

# Pull every genlock-fifo audit line whose ts_present falls in the window (works across any
# number of day-rollovers in the log, no manual midnight-hunting):
awk -F'ts_present=' '{ if (NF>1) { split($2,a," "); ts=a[1]+0; sec=int(ts/1000000000);
  if (sec>=1783742940 && sec<=1783743240) print NR": "$0 } }' "<obs log path>"
```
The printed line NUMBERS then bound a `sed -n 'START,ENDp'` slice for grepping OTHER (non-audit)
lines — reconnect events, `ndi_source_update`, recording start/stop — in the same real window.
Confirmed live (#674 investigation, 2026-07-11): imag-nb's log-printed local time is UTC+2
(Europe/Bratislava CEST) while `ts_present` is a real UTC epoch — cross-check ONE line's printed
`HH:MM:SS` against `date -u -d "@<ts_present/1e9>"` once per session to confirm the offset before
trusting it for a precise window.

## TECHNIQUE — a cheap, low-risk mechanism-hunt repro beats a full E2E harness run for a hypothesis like "does OBS restart degrade imag's reception"

When a full-chain finding (e.g. #674's imag judder-after-restart) needs testing a NARROW causal
hypothesis ("does restarting strih+stream's OBS alone cause X on imag"), don't reach straight for
a full `recording-e2e.sh` BEFORE/restart/AFTER protocol (~15-20 min, needs a clean rig, produces a
QR-decoded judder verdict). Instead: read the SAME live signal the full harness would eventually
measure indirectly (here: imag's own `genlock-fifo audit 'NDI CAM4'` `underruns` counter) directly
from imag-nb's OBS log, take a delta over a short window BEFORE touching anything (a control
baseline), do the isolated action (`launch-obs-genlock.sh --box strih --force` +
`--box stream --force`), then take the SAME delta over a comparable window AFTER. This isolates
the ONE variable (the restart) from everything else a full E2E session also does concurrently
(camera burns, ALL_CAMBOX switching, imag's own scene re-routing) — a live #674 repro this way
found ZERO underrun growth post-restart (cleaner than the control baseline), which meaningfully
narrowed the investigation (ruled out "restart alone" as sufficient) at a fraction of the cost of
a full harness run.

## MAP — the TWO distinct frame-duplication/drop mechanisms in vendored OBS (#726, live-event "15fps-like" stutter)

When a 60fps NDI source feeds a 30fps canvas and the OUTPUT judders (visibly halved motion, "feels
like 15fps" even though the canvas is still mechanically producing 30 frames/sec), there are TWO
architecturally SEPARATE places in `vendor/obs-studio/libobs` this can originate — do not conflate
them; they have different symptoms and different fixes:

**A — per-source genlock release cadence** (`obs-source.c`'s `ready_async_frame`, ~L4750-5050,
the `#401` phase-locked cadence). Runs ONCE PER RENDER TICK PER SOURCE. In STEADY state it presents
exactly ONE matured frame and advances the locked boundary by exactly ONE SOURCE interval
(`next_frame->timestamp + interval`). If the source arrives FASTER than the canvas ticks (a 60fps
source into a 30fps-tick render loop), the per-source queue depth grows until it crosses
`GENLOCK_QDEPTH_RELOCK`, at which point the BACKLOG STORM branch fires (`release = due`) —
presenting the newest due frame and DROPPING (counted in `genlock_dropped_due`) the
matured-but-superseded ones. **Signature: periodic multi-frame content JUMPS (skips), never a
repeated/duplicate frame.** Diagnostic: `genlock_dropped_due`/`genlock_relocks` counters (the
periodic `genlock-fifo audit` log line), or (new, #726) `src/presentation_cadence.rs`'s
`other_steps`/irregular-jump buckets on a REAL (jitter-tolerant, see the recording-decode skill's
own #726 note) cadence read.

**B — canvas-side render-lag frame duplication** (`obs-video.c`'s `video_sleep`/`output_frame`/
`output_video_data`, ~L989-1101). Runs ONCE PER RENDER TICK for the WHOLE canvas (not per-source).
`video_sleep()` computes `count` — how many output-frame slots the CURRENT rendered texture fills
— from whether the render thread hit its wall-clock deadline (`os_sleepto_ns`): a MISS (render
took longer than one canvas interval, e.g. under sustained Multiview/compositing contention — see
`#708`'s "Multiview keeps all 6 raw inputs rendering continuously" finding) makes `count>1`, and
the SAME rendered texture is handed to the encoder `count` times — a literal re-presentation of
the previous frame's pixels. **Signature: a genuinely held/repeated frame (the "hold-then-catch-up"
pattern).** Diagnostic: `video->lagged_frames`/`video->total_frames` — STOCK OBS counters, already
exposed via `obs_get_lagged_frames()`/`obs_get_total_frames()` and via obs-websocket's `GetStats`
as `renderSkippedFrames`/`renderTotalFrames` (this is the SAME "render health" signal
`obs-render-health-metric.md` memory already flags vs the encoder-side `outputSkippedFrames`,
which stays green even when render chokes). Ad-hoc read (no dedicated CLI subcommand exists —
`scripts/obs_phase2.py` has no `stats` verb):
```python
import sys; sys.path.insert(0, "scripts")
import obs_phase2 as op
ws = op._conn("10.77.9.202", "<strih WS password>")  # or "10.77.9.204", "" for stream (no auth)
print(op._rpc(ws, "GetStats"))
ws.close()
```

Both mechanisms plausibly explain "raising the canvas to 60fps mitigated it live" (Mechanism A:
1:1 tick-rate/source-rate alignment removes ALL backlog-driven relock activity structurally;
Mechanism B: a finer render-tick granularity MAY reduce the probability of any single tick missing
its (now shorter) deadline, though this direction is less obviously predicted). #726's own
real-rig data (see the recording-decode skill) is qualitatively more consistent with Mechanism A's
"mostly steady, periodic catch-up" shape than with B's simpler duplicate-then-resume shape, but
this is NOT yet conclusively distinguished — see #726 for the live disambiguation plan (correlate
a `presentation_cadence` read against a `GetStats renderSkippedFrames` DELTA over the SAME
recording window).

**UPDATE (2026-07-13) — Candidate A CONFIRMED + fix implemented, one residual gap found.** Live
data conclusively showed A (uneven hold-then-jump, `duplicate_steps=0` on clean windows — B ruled
out). Fix in `obs-source.c`'s STEADY branch (mirrored in `src/probe/genlock.rs`
`ReleaseCadence::tick`): when `genlock_source_is_integer_multiple(source, canvas_interval)` detects
the source runs at an integer multiple N>=2 of the canvas rate (derived from the front-2-queue
stamp delta, NOT arrival timing), mature every frame up to `boundary + interval/2` and present the
NEWEST, retiring older matured ones into `genlock_dropped_due` — a uniform every-Nth-frame cadence
instead of the old present-oldest-one-per-tick crawl. Deployed via the FAST obs.dll hot-swap
(libobs-only, no struct change → ABI-safe, no ~150-min full build needed — see the #278/#293
precedent this repo already has for this class of change).

**Residual (still open, #726 stays open):** `genlock_source_is_integer_multiple` re-derives N from
scratch EVERY TICK off just the two OLDEST queued frames (`async_frames.array[0]`/`[1]`) — it
returns `false` (falls back to the OLD single-release path) whenever `async_frames.num < 2` or the
front pair isn't strictly monotonic at that instant. Live data (2026-07-13) found ONE camera input
('NDI cam5', feeding CAM1's box) where this flips to `false` for a SUSTAINED period within one
recording window — `genlock_relocks` (the SEPARATE backlog-storm counter, only incremented when
`async_frames.num > GENLOCK_QDEPTH_RELOCK`) climbs continuously (~2/s) on that input while it stays
FLAT on other 60-in-30 inputs ('NDI cam1'/'NDI cam3') in the identical time window — i.e. the same
build, same mechanism, only ONE input affected, asymmetrically. Two live hypotheses, not yet
distinguished: (a) that specific camera box genuinely has noisier per-frame stamp timing at that
moment (a real source-side irregularity), or (b) the per-tick front-2-only re-derivation is simply
too jitter-sensitive and needs to be STICKY (latch the confirmed N per-source, re-derive only after
a genuine relock/gap event). **A sticky-N fix needs a NEW persistent `obs_source_t` field — that is
a STRUCT CHANGE, which forfeits the ABI-safe fast-swap property** (would need the full ~150-min
`windows-genlock.yml` build, not `windows-genlock-fast.yml`) — flag this to whoever picks up the
follow-up so they don't attempt a fast-swap and get confused when it needs `obs-internal.h` too.

**TECHNIQUE — correlate a SPECIFIC verdict window/segment against the strih/stream OBS log via the
verdict.json artifact's own embedded `wall_clock_epoch_s`** (a strih/stream variant of the
imag-nb `ts_present` technique above — same idea, cheaper source field, no manual epoch-window
math needed). Every `full-path-e2e` run uploads a `recording-e2e-full-path` artifact containing
`verdict-<run>.json`; download it (`gh run download <run-id> -n recording-e2e-full-path --dir
<dir>`) and each `all_cambox_continuity.segments[i].residual_events[]` entry already carries a
real `wall_clock_epoch_s` (a UTC unix-epoch integer, no manual `date -u -d` conversion needed) —
use the segment's own `start_ns`/`end_ns` (nanosecond epoch, `/1e9` for seconds) to bound the
window, then grep the corresponding box's OBS log (`win-strih`/`win-stream-snv` MCP `Shell`, the
newest `*.txt` under `$env:APPDATA\obs-studio\logs`) for `genlock-fifo audit '<the specific NDI
input feeding that segment's camera>'` lines whose printed `HH:MM:SS` falls in that UTC window
(strih/stream logs print LOCAL time — Europe/Bratislava CEST, UTC+2 — unlike imag-nb's log, which
uses the SAME local-time convention; convert the epoch to CEST, not UTC, before matching against
the printed timestamp). This is how the #726 residual above was root-caused: identified the ONE
bad segment from the verdict JSON, converted its epoch range to CEST, and found the asymmetric
`relocks` behavior directly in the live OBS log for that exact window.

## #730 — strih's own #501-pattern low-bandwidth multiview twins (`scripts/strih_mv_scenes.py`)

strih never had imag-nb's `#501` "MV Cam N" twin-scene optimization — the built-in Multiview
projector AND strih's own hand-built "Multiview" SCENE (a plain scene whose items are references
to the real "Cam N" scenes, used to feed a physical camera-operator monitor) both rendered every
camera at FULL bandwidth. `scripts/strih_mv_scenes.py` replicates the imag pattern: for every real
"Cam N" scene strih has, it reads that scene's existing "NDI cam\<n>" input's LIVE
`ndi_source_name` (never hardcodes the box's documented INVERTED input-label mapping — see the
top-of-file "strih NDI Input → Camera Mapping" table above) and wraps that exact value in a new
"MV Cam \<n>" twin input (`genlock_monitor=true`, `latency=1`), then:

1. Toggles the built-in-multiview `show_in_multiview` private setting (real scene -> false, twin
   -> true) — the SAME mechanism #501 used on imag.
2. Rewires strih's own "Multiview" SCENE — swaps every scene-item that references a real "Cam N"
   scene for the matching "MV Cam N" twin, ADDING the new item (at the exact same
   position/scale/bounds transform) BEFORE removing the old one, so the live operator monitor
   never drops a tile mid-swap (hot-apply, no OBS restart — strih is production).

Usage: `scripts/strih_mv_scenes.py --host 10.77.9.202 --password <strih WS pw> [--multiview-scene
NAME] [--stats SECONDS]` — idempotent (safe to re-run any time; re-reads the live NDI binding and
re-applies every setting). `--stats N` prints an ad-hoc `GetStats` render-cost delta over N seconds
(no seeding) — the reusable tool the "Rig measurement helpers" section above notes doesn't exist
yet as a CLI verb; this fills that gap for any future before/after render-budget check.

**Live-verified 2026-07-13** (all 6 twins + the Multiview rewire applied to the real strih
production box, confirmed via `GetSceneList`/`GetSceneItemList`/`GetSourcePrivateSettings` and a
screenshot of the live Multiview grid showing the correct twin thumbnails).

**Render-cost finding — HONEST, not the expected win.** A controlled live A/B (same window length,
only the 6 camera tiles toggled between twin/full-bandwidth, everything else held constant —
including an isolated variant with the other 11 non-camera Multiview scenes temporarily hidden)
showed **no measurable difference** between the low-bandwidth twins and the original full-bandwidth
scenes on strih's `averageFrameRenderTime` — see the full write-up + numbers posted to `#726`
(2026-07-13 comment), which is the more relevant open investigation (strih's live-event stutter /
render-contention root-cause hunt) for this data. Short version: strih's Multiview projector also
renders 11 OTHER unrelated scenes untouched by this change, and `averageFrameRenderTime` was
observed climbing (14.75ms -> ~29ms) over the course of testing independent of which camera scenes
were shown — so whatever dominates strih's Multiview render cost is NOT the camera tiles'
NDI bandwidth mode. The twins are still a correct, value-neutral-to-positive change (matches the
proven imag mechanism, adds zero cost, and the underlying win is real at the SOURCE level even if
not visible in the whole-canvas `averageFrameRenderTime` gauge) — just don't cite this as a proven
strih render-cost fix without re-measuring once #726's real driver is found.

**Persistence.** `scripts/strih_mv_scenes.py` itself IS the re-seeding mechanism (mirrors
`imag_scenes.py`'s role for imag — re-run it any time after an OBS reinstall/scene-collection
loss; it is idempotent and self-healing). A live on-box backup of strih's whole scene-collection
directory (`%APPDATA%\obs-studio\basic\scenes\*.json`, all 3 collections including the active
`uplne_orezana.json`) was ALSO taken to `C:\obs-backup\<timestamp>-scenes\` on 2026-07-13, following
the same rollback-backup convention as the genlock DLL deploys above (`C:\obs-backup\<date>\`).
Tests: `tests/python/test_strih_mv_scenes.py` (pure name-mapping/transform-filtering/replacement-
planning/stats-delta logic, no live OBS — mirrors `test_obs_phase2_*.py`'s importlib pattern).

## GOTCHA — editing `~/.config/obs-studio/{global,user}.ini` on a RUNNING OBS box: kill-after-edit races the dying process's own shutdown save (#756, 2026-07-15)

**Never edit `global.ini`/`user.ini` while OBS is still running, then kill it.** The dying
process's OWN shutdown handler writes back the file with its IN-MEMORY config state — which
never saw your edit — silently CLOBBERING it. Live-caught: appended `CloseExistingProjectors=true`
to imag's `user.ini` while OBS (PID X) was running, then `kill -TERM`'d it → the relaunched
process (fresh PID) came up with the key MISSING entirely, because the OLD process's shutdown
save overwrote the file with its stale content the instant it received the signal. **Correct
order: STOP OBS first (confirm `pgrep -x obs` empty), THEN edit the config file, THEN start OBS.**
On imag specifically, also `systemctl stop imag-obs-watchdog.service` first — the watchdog
auto-relaunches OBS on death (tier-a), which would otherwise race your edit exactly the same way.

**Second, independent gotcha in the SAME investigation — libobs's ini parser is NOT Qt's
QSettings, and a DUPLICATE `[section]` header is silently unreachable.** `util/config-file.c`
(a custom parser, `util/uthash.h` just redefines a couple of upstream `uthash`'s macros — NOT a
1000+-line hash-table implementation itself, that lives at the SYSTEM `uthash-dev` package)
stores sections in a uthash table keyed by name; adding a SECOND `[BasicWindow]` header (e.g. by
naively `printf '\n[BasicWindow]\nKey=value\n' >> file` when the file ALREADY has a
`[BasicWindow]` section) does NOT merge into the first section — `config_get_bool`/
`config_find_item` only ever resolve the FIRST-inserted section, so a key seeded into the later
duplicate is silently unreachable, and gets DROPPED ENTIRELY the next time OBS itself saves the
file (its save only ever writes back what it loaded). **Always INSERT a new key into an EXISTING
section instead of appending a new one:**
```bash
if grep -q '^\[BasicWindow\]$' "$f"; then
    sed -i '0,/^\[BasicWindow\]$/s//[BasicWindow]\nNewKey=value/' "$f"   # GNU sed first-match-only idiom
else
    printf '\n[BasicWindow]\nNewKey=value\n' >> "$f"                     # only when truly absent
fi
```
See `scripts/setup-imag.sh`'s `seed_ini()` for the shipped version of this pattern (the
`CloseExistingProjectors` seed) and `tests/harness_projector_count_756.rs` for a functional
(execution) test that actually RUNS the extracted `seed_ini()` body against synthetic fixtures —
a purely textual `body.contains(...)` check cannot catch this class of bug (the literal string is
present in the script either way; only running it against an ALREADY-populated fixture file
reveals the duplicate-section defect).

## GOTCHA — a live gdb breakpoint on a HOT render/shutdown-path function can itself trigger a real wedge-reboot (#756, 2026-07-15)

Live-caught TWICE this session: setting a gdb breakpoint on `config_get_bool` (called from many
places, including per-UI-update paths) on imag's running OBS process, with `commands`/`continue`
auto-resuming on every hit, stalled the render thread badly enough that the watchdog's tier-b
wedge detector fired a REAL reboot (`fps=43.5` observed, matching genuine wedge thresholds) — the
debugger itself became the load, not just an observer. `obs_enter_graphics`-family functions are
called even MORE often than `config_get_bool`, so breakpointing anywhere in that call graph live
is HIGHER risk still. **Before attaching gdb to a running OBS process on ANY rig box, consider:
(1) is the target function on a per-frame or per-UI-tick hot path — if so, expect real
performance impact, not just "a debugger is attached"; (2) can the same question be answered via
a STANDALONE, OFF-BOX reproduction instead** (compile just the relevant `.c`/`.rs` files with
minimal stubs and feed them synthetic input — see the `config_probe` throwaway harness this
investigation built: `gcc -std=gnu11 -I vendor/obs-studio/libobs -c
vendor/obs-studio/libobs/util/config-file.c` + stub `os_*`/`bmem`/`dstr` functions + a tiny
`config_open_string()`-driven `main()` — this is 100% safe, zero rig risk, and answered the
question this gdb session was chasing definitively once written); **(3) if gdb on the live box is
truly unavoidable, prefer a FILTERED conditional breakpoint that skips fast for 99% of hits AND
budget for the possibility of a wedge-reboot** (the watchdog's alarm-only mode, if armed, at least
won't auto-reboot out from under you — but the render-thread stall itself still happened and still
degraded whatever you were trying to measure). Never assume "just attaching + reading a few
frames" is free on a box with an active wedge-reboot watchdog.

## GOTCHA — `libobs-opengl.so.30` is a SEPARATE deploy artifact from `libobs.so.30` on imag's Linux hot-swap; it was silently excluded for 11+ days (#756, 2026-07-15)

`vendor/obs-studio/libobs-opengl/CMakeLists.txt` builds `libobs-opengl` as its OWN
`add_library(... SHARED)` target — a genuinely separate `.so` from `libobs.so.30`, even though
both live under the same `vendor/obs-studio/` tree and both get produced by the SAME
`linux-genlock.yml` build (the bundle stage copies the FULL `cmake --install` rundir, so
`libobs-opengl.so.30` is ALWAYS correctly present in `BUNDLE_MANIFEST.json` — this was a pure
DEPLOY-script gap, never a build gap). `scripts/setup-imag.sh`'s genlock hot-swap (step 12,
since #460/#499) only ever named `LIBOBS_REAL`/`DISTROAV_REAL`/`OBS_FRONTEND_REAL` — so a change
confined to `vendor/obs-studio/libobs-opengl/*.c` (e.g. #756 Fix B, the X11/EGL client-size
cache in `gl-x11-egl.c`) could ship, build clean, get "deployed" (the SHA marker updates,
`NOOP_VALID` reports success), and STILL never actually reach imag — live-confirmed: the loaded
file was dated 11 days stale while `GENLOCK_BUILD_SHA.txt` claimed the current dev HEAD. Fixed
in #756 (`2789f46c8`): `LIBOBS_OPENGL_REAL="/usr/lib/x86_64-linux-gnu/libobs-opengl.so.30"` is
now a 4th swapped component with the SAME manifest-verify/backup/install/SONAME-check treatment
as the other three, and the #472 no-op re-verify re-hashes it too. **The lesson for any FUTURE
Linux-side vendored-OBS change:** before assuming a change is covered by the existing hot-swap,
check WHICH `.so`/binary the touched file compiles into (`grep -rn "add_library\|add_executable"
vendor/obs-studio/**/CMakeLists.txt`) and confirm that exact output path is named in
`scripts/setup-imag.sh`'s step 12 — do not assume "it's under vendor/obs-studio, the hot-swap
must cover it".

## GOTCHA — OBS's own crash-recovery `.sentinel` clearing is needed for the LINUX watchdog's auto-relaunch too, not just the Windows launch wrapper (#756, 2026-07-15)

The Windows `launch-obs-genlock.sh` wrapper has always cleared `%APPDATA%\obs-studio\.sentinel\*`
before relaunching (documented earlier in this file). `imag-obs-watchdog.py`'s tier-a
`relaunch_obs()` (the auto-relaunch-a-dead-OBS recovery path) never had the Linux equivalent —
it always launches a fresh `/usr/bin/obs --disable-shutdown-check` WITHOUT first clearing
`~/.config/obs-studio/.sentinel/*`. A tier-a relaunch follows a DEAD obs process — by
construction never a clean exit — so the crashed run's sentinel is ALWAYS still present, and
OBS's own crash-recovery check hangs the freshly-launched process at `"Crash or unclean shutdown
detected"` in its log (near-idle CPU, no further progress) instead of actually starting. Live-hit
during #756's own hot-swap (which SIGKILLs the old process before installing new bytes): the
watchdog's own auto-relaunch attempt hung exactly this way, `ws_up: false` in its own alarm
record. Fixed by adding a `clear_obs_sentinels()` step to `relaunch_obs()` (glob + best-effort
remove, never blocking the relaunch attempt on a removal failure). `scripts/imag-obs-watchdog.py`
is now tracked in git for the first time — read it there, not on the box, when investigating this
class of issue. **If you ever manually SIGKILL/kill OBS on imag (or any box) as part of a
hot-swap/investigation, always clear the sentinel before the NEXT launch — whether you relaunch
by hand or let the watchdog do it.**

## GOTCHA — a Windows OBS "Projector" window is a THREAD of the SAME obs64.exe process, not a separate PID; killing it by title-matched PID kills OBS itself (2026-07-15)

`Get-Process | Where-Object {$_.MainWindowTitle -like "*Projector*"}` on a Windows OBS box can
return a PID whose `MainWindowTitle` happens to be `"Projector - Program"`/`"Projector -
Multiview"` at that instant — but that PID is `obs64.exe`'s OWN process id (its main window
handle just currently reports the LAST-focused/foreground window's title, which can be a
projector). `Stop-Process -Id <that PID> -Force` therefore SIGKILLs the whole OBS process, not
just the projector window — live-confirmed on strih (2026-07-15): killing what looked like a
stray "Projector - Program" window actually crashed OBS entirely, which AHK then auto-respawned
(racing a separate manual relaunch attempt into a genuine two-obs64-instances collision).
**Never target a Windows OBS projector for closure by PID from a title match.** To close ONLY a
projector window (never the OBS process), drive it through OBS WebSocket / the UI (there is no
`CloseProjector` RPC in obs-websocket v5 as of this writing — closing a specific projector
window is a UI-only action, e.g. Alt+F4 while it has focus, or just leave it open) rather than a
blind `Stop-Process` on any PID discovered via a window-title search.

## GOTCHA — OBS's BUILT-IN Multiview grid PROJECTOR and a user-created SCENE literally named "Multiview" are two UNRELATED things — do not conflate them (2026-07-15)

`OpenVideoMixProjector {videoMixType: OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW}` opens OBS's
BUILT-IN system feature — a grid of THUMBNAILS of every scene, rendered continuously while the
projector window is open, independent of program/preview state (this is what the render-divisor
cadence floor #278/#293/#756 all throttle). strih ALSO happens to have a hand-built SCENE named
literally `"Multiview"` (#730 — a plain scene whose items reference other scenes, used to feed a
physical camera-operator monitor) — a COMPLETELY SEPARATE object that only renders when IT is
shown (via `OpenSourceProjector {sourceName: "Multiview"}`, or being on program/preview itself).
Opening a projector of the SCENE named "Multiview" does NOT engage the built-in Multiview
system feature, and vice versa — confirmed live: reopening the built-in Multiview grid after an
OBS restart does NOT require (and is unrelated to) the "Multiview" scene's own projector. If you
need "every camera input to stay actively rendering regardless of program/preview" (the
liveness-probe precondition several #758 mechanisms rely on), that's the BUILT-IN Multiview grid
projector (`OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW`) — the operator's physical monitor feed (the
"Multiview" SCENE) is a separate, unrelated concern. The projector's real window title for the
built-in feature is exactly `"Projector - Multiview"`.

## strih per-camera liveness probe target is PER-BOX and has changed convention twice (#758→#761→#763) — check the CURRENT wiring before reusing this pattern elsewhere

`recording-e2e.sh`'s three #758 camera-liveness mechanisms ([1/8] preflight,
`preflight_mv_reverify` sender-bounce reverify, the in-run freeze watch) all probe strih over
`frozen-camera-gate.py --sources`. **As of #756/#761 (2026-07-15) this probes the MAIN
`"NDI camN"` inputs on strih** (`strih_mv_scenes.py --reattach` matches: it re-applies the MAIN
input's own `ndi_source_name`). This works with NO `--warm-settle` (still passed `0`, i.e.
warm-up disabled) because the built-in OBS Multiview grid projector — see the GOTCHA above —
keeps every "Cam N" scene, and hence its "NDI camN" input, continuously rendering regardless of
program/preview state, as long as that projector stays open (the rig's normal/expected state).
This SUPERSEDES the original #758 design (probing the low-bandwidth `"MV NDI camN"` clone
twins, #730) — those clone scene-items were disabled then REMOVED from strih's "MV Cam N"
scenes entirely (a user-directed same-source switch, #761, KEPT — do not re-enable them).
**imag is DIFFERENT and still uses the clone model** (full 1080p×7 doesn't fit its render
budget) — #763 tracks unifying the two boxes onto one model later; there is no existing
imag-hosted frozen-camera-gate call site as of this writing, but if one is ever added it must
target the clones, not the mains, mirroring strih's own split. Before reusing/extending ANY of
these three mechanisms, re-verify which convention is currently wired on the target box —
`GetSceneItemList {sceneName: "MV Cam N"}` / a live `GetInputSettings` on the "MV NDI camN"
input's `ndi_source_name` will tell you which mode is currently live.
