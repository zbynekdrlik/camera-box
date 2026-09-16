---
paths:
  - "scripts/render-freeze-alert-watchdog.sh"
  - "scripts/render_freeze_decision.py"
  - "systemd/render-freeze-alert-watchdog.*"
  - "tests/python/test_render_freeze_decision_1320.py"
  - "tests/python/test_relock_bursts_gather_1320.py"
---

# dev1-side render-freeze / relock-storm alert watchdog (#1320)

The guardrail for issue 1320's cure (bundle `02b53180b`): page in ~10 min if the strih PROGRAM
render freeze — or its downstream stream relock storm — ever **recurs**, instead of the ~90-min
silent on-air A/V drift issue 1318 first surfaced. Same dev1 alert-watchdog family as audio-lag
(#1226) / bundle-state (#732) / genlock-lock (#1299).

## Two facets, two arms (both read off `:8899/bundle-state.json`)

| Facet (gather) | Meaning | Arm | Pages when |
|---|---|---|---|
| `program_render_lagged` (+ `_age_s`) | MAX `program-render-audit lagged` over the #1222 bounded tail — a `lagged>0` window == a PROGRAM render-thread freeze | RENDER | `lagged >= LAGGED_FLOOR` **and** age fresh → `RENDER_FREEZE` |
| `relock_bursts` (+ `_age_s`) | issue 1318's `summarize_relock_bursts` PORTED to the gather (`relock_bursts_from_log`): MAX per-input burst count (≥8 relocks within 1 s) | RELOCK | `bursts >= 1` **and** age fresh → `RELOCK_STORM` |

`relock_bursts_from_log` is a **Python mirror** of the Rust `summarize_relock_bursts`
(`src/jitter_audit.rs`) — the summarizer is NOT re-implemented across languages beyond this one
mirror (the `ndi_halving_decision` / #1199 precedent), and `test_relock_bursts_gather_1320.py`'s
parity block feeds the SAME synthetic event sequences the Rust tests use and asserts identical
`bursts`/`max_per_second`, so the Python mirror can never silently drift.

## The load-bearing discriminator — a fresh freeze vs a relaunch-window lag

A relaunch **legitimately** lags a little (observed band `prl 1/2/11`, e.g. stream `prl=2`,
resolume `prl=1`) vs the `228` real freeze (the 17:04 partial was `61`). The RENDER arm therefore
needs BOTH:

- a **magnitude FLOOR** (`RENDER_FREEZE_LAGGED_FLOOR`, default **30**) — above the relaunch band with
  margin, below the smallest genuine freeze. This excludes a relaunch startup-lag by MAGNITUDE, so it
  is more robust than a "log younger than 3 min" uptime guess (no `obs_start` bundle-state facet
  exists, and the burn-reconcile watchdog's own `GetStats.renderTotalFrames` restart signal is over
  the OBS WebSocket — unreachable to a dev1-side `:8899`-only watchdog). Rejected a pure `lagged>0`
  gate: a relaunch lag is sustained across passes, so the 2-pass confirm would confirm it.
- a **freshness bound** (`RENDER_FREEZE_FRESH_AGE_S`, default **600 s** ≈ 2× the 5-min cadence) — a
  freeze that scrolled deep into the tail / an old relaunch lag (stream's live `age~4992 s`) is
  stale, no page. The bound spans the 2-pass confirm so a real recent event still pages.

The RELOCK arm mirrors it (`RENDER_FREEZE_MIN_BURSTS` default 1, `RENDER_FREEZE_RELOCK_FRESH_AGE_S`
default 600). `"0"` (relock telemetry live, no storm) is HEALTHY; an **absent** relock facet is the
STEADY STATE (relock lines only appear during a storm) → UNKNOWN → no page.

## PRODUCTION-CRITICAL — time-bucketed re-ping (#1308)

A recurrence silently desyncs the on-air A/V, so **both arms** carry a TIME-BUCKETED `--dedup-key`
(`watchdog_notify_key "render-freeze-<box>"` / `"relock-storm-<box>"`) — within a bucket they
card-edit (no flood), each new bucket re-pings "dokolečka" while it persists. The watchdog is on the
`_PRODUCTION_CRITICAL_TIME_BUCKETED` allowlist in `tests/python/test_notify_dedup_key_sweep_1206.py`
(invariants A: a dedup-key is present, B: no ✅ recovery ping, still hold). Registered in
`scripts/lib/watchdog-roster.sh` (`render-freeze-alert-watchdog.timer:core`) for the #1319 handover
check.

## Fleet roster + never-false-page

Roster = the NEW `render-freeze` facet in `scripts/lib/obs-fleet.sh`
(`obs_fleet_boxes render-freeze` = **strih stream resolume strih-lx**). resolume runs the cg-obs
PROGRAM render + receives, so it IS in scope. The **only** page condition is a
SUCCESSFULLY-FETCHED positive reading, so a dark box (a not-yet-provisioned strih-lx, a traveling
resolume that is away, a `:8899` outage) → `box_reachable=0` → **SKIP** → deferred to #732/#1001, no
page. This is why resolume (traveling) needs **no** `is_home` gate here, and why there is no
reference-anchor / dev1-side-outage guard.

## DETECTION ONLY — ships DISABLED

The cure for a recurrence is a full-bundle redeploy / an OBS relaunch — an owner/supervisor call, so
there is NO auto-action; recovery is machine-channel log-only (never a phone ping,
`.claude/rules/watchdog-notify-dedup.md` #1206). The units are committed but the SUPERVISOR installs
+ enables them (README supervisor procedure). Live read-only `--dry-run` seam:
`scripts/render-freeze-alert-watchdog.sh --dry-run` (verified 16.9. against strih/stream/resolume =
HEALTHY, strih-lx = SKIP).
