# av-step-alert-watchdog — install note (#1267)

The dev1-side alert watchdog (`scripts/av-step-alert-watchdog.sh`) is the missing EARLY WARNING for
the **2026-09-01** A/V-gate failure class: the mastered Dante feed into the stream box's DVS `mbc`
source got ≈ −50…−90 ms later at 17:50–18:10 local — an **UPSTREAM audio-chain latency STEP**, NOT
the stream-OBS `ts_lag` flap (issue 1265's band watch) and NOT the video path. The genlock pin
`NDI 2ME PGM` held 926 and strih had no reboot, yet the E2E A/V gate residual went −77/−126/−111
**three hours later**. The stream av-sync dock already MEASURED the shift — its `LOCK-CORRECT
SUGGESTED genlock_latency_ms_src <pin> -> <new>ms (measured offset=<X>ms)` line (monitor-only,
~2/min — a live, E2E-independent, restart-independent A/V trend) — but nothing off the box read it.

It reads the **`av_offset_*` facets** `bundle_state_gather` (#1267) now exposes on the stream box's
`:8899/bundle-state.json` (the RECENT-vs-BASELINE median measured offset + the current genlock pin +
a pin-stability flag + the freshness age + per-window sample counts, all parsed from the SAME #1222
bounded head+tail read — no second log scan), polling the box from a **dev1 systemd --user timer**
with `curl` (the ops-SKILL-mandated method — an MCP-side `Invoke-WebRequest` hangs even when the
server logs a prompt 200). It is the upstream-A/V-step sibling of `audio-lag-alert-watchdog` (#1226,
which is the OBS-internal ts_lag HEALTH signal — a DIFFERENT axis), `bundle-state-alert-watchdog`
(#732) and `network-reach-alert-watchdog` (#1001).

## The genlock pin is a COVARIATE, never subtracted

Verified LIVE (2026-09-02): a pin jump 976→1024 (E2E test-latency churn) left the raw measured
offset ~unchanged, so `offset − pin` would read a −48 ms **phantom** step. The box therefore reports
a `av_offset_pin_stable` flag instead; the decision judges a STEP **only across a CONSTANT-pin
window** and HOLDs (REPIN, report-only, no page) whenever the pin moved in the analyzed span — which
is exactly the E2E test-latency window and a #856/operator apply. The real 2026-09-01 episode had a
constant pin (926 held for hours), so the constant-pin detector catches it cleanly.

## DETECTION ONLY — report-only, no auto-action

The cure is a **live-box OBS restart** (a destructive owner-call, `no-destructive-remote-actions.md`)
or an **upstream Dante / mastering investigation** — an owner decision on the ticket, not automation,
exactly like issue 1265's band watch and the #1226 lag watch. So this watchdog is **alert-only**: on
a confirmed step it pages a report-only ⚠️ (throttled), and **recovery is log-only / machine-channel**
(never a phone ping — `.claude/rules/watchdog-notify-dedup.md` #1206; the alert carries a stable
`--dedup-key av-step-<box>` so a persisting step EDITS the one card instead of re-pinging).

## Separation of concerns — it never false-pages, and never double-pages a down box

The **only** page condition is a SUCCESSFULLY-FETCHED positive STEP reading, so:

- A box whose `:8899` is not fetchable this pass (box down, or `:8899` down) classifies **SKIP** —
  deferred to `bundle-state-alert-watchdog` (#732) / `network-reach-alert-watchdog` (#1001).
- A box up but with **no `av_offset_*` facet yet** (box not upgraded, or no dock line in the tail),
  OR **too few dock samples** in either window, classifies **UNKNOWN** — no reading to judge, no page.
- A box whose dock series has **STOPPED while the OBS log kept advancing** (freshest line older than
  `AV_STEP_STALE_THRESHOLD_S` behind the log head) classifies **STALE** — surfaced distinctly on the
  machine channel, never a phone page (absence is #732/#1001 territory).
- A box whose **pin moved** across the analyzed span classifies **REPIN** — report-only, no page.
- Because a page requires a fetched positive reading, a **dev1-side path outage** makes every fetch
  fail → SKIP → no page. That is why this watchdog needs **no** reference-anchor/outage guard.

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo / this PR. Before it
runs unattended, the **SUPERVISOR** installs it, live-verifies it (below), and only then enables the
timer. No box-side change is made by this ticket.

## Watched-box scope — stream only by default

The av-sync dock measured-offset series is a **stream-only** signal: strih logs `av-sync-dock: ASRC
section unavailable -- source 'mbc' not found on this box`, so it never carries the facet (it would
always classify UNKNOWN there). The default `AV_STEP_BOXES="stream|10.77.9.204"` watches stream
only; add strih via the env var only if that ever changes.

## Conservative gates (why it won't thrash or spam)

- Requires **2 consecutive confirmations** (`AV_STEP_CONFIRM_THRESHOLD`, default 2) before it
  alerts — one blipped median is observe-only. A HEALTHY read resets the counter; a
  SKIP/UNKNOWN/STALE/REPIN pass HOLDS the counter (an unmeasured/held pass neither advances nor
  resets it).
- The step threshold is `AV_STEP_THRESHOLD_MS` (default **45 ms**) — normal 10-min dock medians
  wander ±30 ms within an hour, the real step was −60…−90 ms, so 45 cleanly separates them. Repeat
  alerts are throttled to once every `AV_STEP_ALERT_THROTTLE_PASSES` passes (default 12 ≈ 1h) while
  the same box stays stepped.
- Each window needs `AV_STEP_MIN_SAMPLES` (default 6 ≈ 3 min at the ~2/min dock cadence) or the box
  is UNKNOWN — never a step off thin data.
- A recovery ("back to normal") line is logged once (machine-channel) when a box we paged for
  returns to a healthy median.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — fetch + decide + LOG only, NEVER alert:
scripts/av-step-alert-watchdog.sh --dry-run             # inspect the per-box verdict + decision

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/av-step-alert-watchdog.service ~/.config/systemd/user/
cp systemd/av-step-alert-watchdog.timer   ~/.config/systemd/user/
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer:
#    a) with the stream box healthy -> a manual pass must report HEALTHY/UNKNOWN and take NO action:
systemctl --user start av-step-alert-watchdog.service ; journalctl --user -u av-step-alert-watchdog -n 50
#    b) simulate a step: serve a crafted bundle-state with a large |recent_med - base_med| at a
#       constant pin (or temporarily lower AV_STEP_THRESHOLD_MS below the live |step|), confirm two
#       consecutive passes page with the correct box + step + pin, then confirm the recovery line is
#       logged (machine-channel) once it clears. Confirm a pin-change bundle-state reads REPIN (no page).

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now av-step-alert-watchdog.timer
systemctl --user list-timers | grep av-step-alert-watchdog

# Disable later:
systemctl --user disable --now av-step-alert-watchdog.timer
```

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `AV_STEP_THRESHOLD_MS` | `45` | `\|recent_med - base_med\|` (ms) above which a box has STEPPED |
| `AV_STEP_MIN_SAMPLES` | `6` | dock samples required in EACH window before a step is judged (else UNKNOWN) |
| `AV_STEP_STALE_THRESHOLD_S` | `300` | dock-series age (s) behind the OBS log head above which it is STALE (surfaced distinctly, never paged). Matches the box-side `bundle_state_gather.AV_OFFSET_STALE_AFTER_S` |
| `AV_STEP_CONFIRM_THRESHOLD` | `2` | consecutive STEP readings before paging |
| `AV_STEP_ALERT_THROTTLE_PASSES` | `12` | passes between repeat alerts (≈1h at the 5-min cadence) |
| `AV_STEP_BOXES` | `stream\|10.77.9.204` | `name\|ip` pairs to watch (stream-only by default) |
| `AV_STEP_CURL_TIMEOUT` | `10` | `:8899` HTTP fetch timeout (s) |
| `AV_STEP_ALERT_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-av-step-alert.state` | per-box confirm/throttle/recovery state |

## What this does NOT do

- It makes **no box-side change** and takes **no auto-action** — the cure (an OBS restart / an
  upstream audio-chain fix) is an owner call; this watchdog only detects and alerts, report-only.
- It does not diagnose the OBS-internal `ts_lag` HEALTH (that is #1226's axis) — those are two
  different signals: a run AFTER the 2026-09-01 stream-OBS restart had a FLAT ~85 ms `ts_lag` band
  yet still measured a −111 ms A/V residual across all 7 cams, because the upstream step this
  watchdog detects is independent of the OBS ts_lag flap.
