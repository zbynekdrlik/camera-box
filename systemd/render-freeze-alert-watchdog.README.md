# render-freeze-alert-watchdog — install note (#1320)

The dev1-side alert watchdog (`scripts/render-freeze-alert-watchdog.sh`) is the **guardrail** for
issue 1320: on 15.9.2026 a scene-switch-coincident DistroAV reattach whose blocking
`NDIlib_recv_destroy` ran on strih's OBS **graphics thread** froze the PROGRAM render ~7.5 s
(`program-render-audit lagged=228 avg_frame_ms=782`) → the `2ME PGM` NDI output was starved → the
stream receive FIFO underran → a **462-relock overshoot storm** → the on-air video sat **+2/+3 frames
late for ~40 min**. The owner only noticed ~90 min later via the av-sync dock offset (issue 1318).
The **cure** (bundle `02b53180b`) moves the teardown off the graphics thread. This watchdog pages in
**~10 min** if the freeze — or its receiver-side storm — ever **recurs**, instead of a silent drift.

It reads two facets `bundle_state_gather` exposes on each box's `:8899/bundle-state.json` (parsed
from the SAME #1222 bounded head+tail read — no second log scan):

- **`program_render_lagged` (+ `_age_s`)** — the MAX `program-render-audit lagged` over the tail. A
  `lagged>0` window is a PROGRAM render-thread freeze. The **RENDER** arm pages `RENDER_FREEZE` on
  `lagged >= LAGGED_FLOOR` **and** a fresh age.
- **`relock_bursts` (+ `_age_s`)** — issue 1318's `summarize_relock_bursts` ported to the gather
  (`≥8` relocks within 1 s on one input = a FIFO overshoot storm). The **RELOCK** arm pages
  `RELOCK_STORM` on `bursts >= 1` **and** a fresh age.

It polls the **`render-freeze` fleet facet** (`obs_fleet_boxes render-freeze` = **strih stream
resolume strih-lx**) from a dev1 systemd `--user` timer with `curl` (the ops-SKILL-mandated method —
an MCP-side `Invoke-WebRequest` hangs even when the server logs a 200). Sibling of
`audio-lag-alert-watchdog` (#1226) / `bundle-state-alert-watchdog` (#732).

## The load-bearing discriminator: a fresh freeze vs a relaunch-window lag

A relaunch **legitimately** lags a little. Live reads at ship time: strih `prl=0`, stream `prl=2`
(age ~4992 s), resolume `prl=1` (age ~354 s) — vs the **228**-lagged real freeze. So the RENDER arm
uses **two** gates:

- **A magnitude FLOOR** (`RENDER_FREEZE_LAGGED_FLOOR`, default **30**) — above the relaunch band
  (`1/2/11` observed) with margin, below the smallest genuine freeze (the 17:04 partial was `61`, the
  18:27 severe `228`). A relaunch startup-lag is excluded by magnitude, no page.
- **A freshness bound** (`RENDER_FREEZE_FRESH_AGE_S`, default **600 s** ≈ 2× the 5-min cadence) — a
  freeze that scrolled deep into the tail / an old relaunch lag (stream's 4992 s) is stale, no page.

The RELOCK arm uses `RENDER_FREEZE_MIN_BURSTS` (default 1) + `RENDER_FREEZE_RELOCK_FRESH_AGE_S`
(default 600 s).

## PRODUCTION-CRITICAL — time-bucketed re-ping (#1308)

A recurrence silently desyncs the on-air A/V, so **both arms** carry a **TIME-BUCKETED** `--dedup-key`
(`watchdog_notify_key "render-freeze-<box>"` / `"relock-storm-<box>"`) — within a bucket they
card-edit (no flood), each new bucket re-pings "dokolečka" while the state persists. This watchdog is
on the `_PRODUCTION_CRITICAL_TIME_BUCKETED` allowlist in
`tests/python/test_notify_dedup_key_sweep_1206.py`.

## DETECTION ONLY — no auto-action

The cure for a recurrence is a **full-bundle redeploy** / an **OBS relaunch** — a supervisor/owner
call (`no-destructive-remote-actions.md`), never automation. So this watchdog is **alert-only**, and
**recovery is machine-channel log-only** (never a phone ping — `.claude/rules/watchdog-notify-dedup.md`
#1206).

## It never false-pages, and never double-pages a down box

The **only** page condition is a SUCCESSFULLY-FETCHED positive reading, so:

- A box whose `:8899` is not fetchable this pass (box down, or `:8899` down) classifies **SKIP** —
  deferred to `bundle-state-alert-watchdog` (#732) / `network-reach-alert-watchdog` (#1001). This is
  why **resolume** (traveling) is safe in the roster with **no** `is_home` gate: a dark resolume just
  SKIPs. (Confirmed live: **strih-lx** not yet provisioned → SKIP.)
- A box that is up but has **no facet** yet (a stock OBS / a steady state with no relock line)
  classifies **UNKNOWN** — no reading to judge, no page.
- Because a page requires a fetched positive reading, a **dev1-side path outage** makes every fetch
  fail → SKIP → no page. No reference-anchor / outage guard needed.

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this PR. Before it runs
unattended, the **SUPERVISOR** installs it, live-verifies it (below), and only then enables the timer.
No box-side change is made by this ticket.

## Conservative gates

- **2 consecutive confirmations** (`RENDER_FREEZE_CONFIRM_THRESHOLD`, default 2) before either arm
  alerts. A HEALTHY read clears that arm's counter; a SKIP/UNKNOWN pass holds it.
- Repeat alerts are throttled once every `RENDER_FREEZE_ALERT_THROTTLE_PASSES` passes (default 12 ≈
  1 h) while the same box+arm stays firing.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run against the LIVE rig (read-only; no page, no state mutation):
scripts/render-freeze-alert-watchdog.sh --dry-run
#    Expect every reachable box HEALTHY (steady state) and any dark box SKIP.

# 2. Install the units into the dev1 user unit dir and enable the timer:
mkdir -p ~/.config/systemd/user
cp systemd/render-freeze-alert-watchdog.service systemd/render-freeze-alert-watchdog.timer \
   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now render-freeze-alert-watchdog.timer

# 3. Confirm it fires + reads the facets:
systemctl --user list-timers | grep render-freeze
journalctl --user -u render-freeze-alert-watchdog.service -n 40 --no-pager
```

To force a synthetic verification without waiting for a real freeze, point the watchdog at a fixture
JSON via `RENDER_FREEZE_BOXES`/a local HTTP stub, or lower `RENDER_FREEZE_LAGGED_FLOOR` on a `--dry-run`
against a box carrying a small relaunch lag (it will log `RENDER_FREEZE` in dry-run without paging).
