# genlock-lock-alert-watchdog — install note (#1299)

The dev1-side alert watchdog (`scripts/genlock-lock-alert-watchdog.sh`) makes the in-OBS genlock
LOCK state (#1298) **fleet-visible**: the #1298 indicator is seen only by an operator standing at
each box's statusbar, so a box that silently leaves LOCKED (clock undisciplined, a genlock NDI
output no longer stamping wall time, inputs unlocked) went unnoticed until someone looked. This
watchdog closes that gap — the genlock sibling of `bundle-state-alert-watchdog` (#732),
`network-reach-alert-watchdog` (#1001), and `audio-lag-alert-watchdog` (#1226).

It reads the **`genlock_lock` facet** `bundle_state_gather` (#1299) now exposes on each box's
`:8899/bundle-state.json` — the **DECIDED** three-state verdict (LOCKED / DEGRADED / UNLOCKED) the
#1298 statusbar widget emits on its versioned `genlock-lock-json:` OBS-log line, parsed from the
SAME #1222 bounded head+tail read every other facet uses (no second log scan). It polls the fleet
from a **dev1 systemd --user timer** with `curl` (the ops-mandated method — an MCP-side
`Invoke-WebRequest` hangs even when the server logs a prompt 200). The box roster is derived from
the ONE declared fleet list (`scripts/lib/obs-fleet.sh`'s `genlock-lock` facet, #1296): **strih,
stream, imag, resolume** — resolume is a TRAVELING CG box and is paged ONLY while
`obs_fleet_is_home resolume` holds (away → skipped entirely, never a false page).

## DETECTION ONLY — no auto-action

The cure for a genuinely unlocked box (restart OBS / dantesync, an NDI receiver reattach) is a
rig-ops decision, not something a dev1 timer should drive blind. So this watchdog is **alert-only**:
on a confirmed UNLOCKED/DEGRADED it pages (throttled), and **recovery is log-only / machine-channel**
(never a phone ping — `.claude/rules/watchdog-notify-dedup.md` #1206; the alert carries a stable
`--dedup-key genlock-lock-<box>` so a persistent state EDITS the one card instead of re-pinging,
while a DEGRADED→UNLOCKED escalation — a genuinely different verdict in the throttle signature —
re-fires to update the card text).

## Separation of concerns — it never false-pages, and never double-pages a down box

The **only** page condition is a SUCCESSFULLY-FETCHED facet reading UNLOCKED/DEGRADED, so:

- A box whose `:8899` is not fetchable this pass (box down, or `:8899` down) classifies **SKIP** —
  deferred to `bundle-state-alert-watchdog` (#732) / `network-reach-alert-watchdog` (#1001). No
  genlock page for a down box, no duplicate.
- A box that is up but has **no `genlock_lock` facet yet** (a stock OBS, or no `genlock-lock-json:`
  line in the log tail yet) classifies **UNKNOWN** — no reading to judge, no page, NEVER a false
  UNLOCKED.
- Because a page requires a fetched positive reading, a **dev1-side path outage** makes every fetch
  fail → SKIP → no page. That is why this watchdog needs **no** reference-anchor/outage guard
  (unlike bundle-state #732, which restarts tasks and pages on a DOWN box and therefore does need
  one).

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo / this PR. Before it
runs unattended, the **SUPERVISOR** installs it, live-verifies it (below), and only then enables the
timer. No box-side change is made by this ticket.

## Conservative gates (why it won't thrash or spam)

- Requires **2 consecutive confirmations** (`GENLOCK_LOCK_CONFIRM_THRESHOLD`, default 2) before it
  alerts — one blipped reading (a reload, a one-tick relock) is observe-only. A HEALTHY (LOCKED)
  read resets the counter; a SKIP/UNKNOWN pass HOLDS it (an unmeasured pass neither advances nor
  resets it).
- Repeat alerts are throttled to once every `GENLOCK_LOCK_ALERT_THROTTLE_PASSES` passes
  (default 12 ≈ 1h) while the same box stays in the same verdict.
- A recovery ("back to LOCKED") line is logged once (machine-channel) when a box we paged for
  returns to LOCKED.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass against a CAPTURED fixture — fetch + decide + LOG only, NEVER alert.
#    The GENLOCK_LOCK_FETCH_CMD seam replaces the live curl, so no box is needed. The acceptance
#    criterion ("the dev1 watchdog --dry-run classifies a captured UNLOCKED fixture"):
cat > /tmp/unlocked.json <<'JSON'
{"obs_version":"32.1.2","genlock_lock":{"state":"UNLOCKED","reason":"clock","n_inputs":7,"n_locked":0,"source":"log"}}
JSON
printf '#!/usr/bin/env bash\ncat /tmp/unlocked.json\n' > /tmp/fetch.sh ; chmod +x /tmp/fetch.sh
GENLOCK_LOCK_FETCH_CMD=/tmp/fetch.sh GENLOCK_LOCK_BOXES="strih|10.77.9.202" \
  scripts/genlock-lock-alert-watchdog.sh --dry-run   # must log "WOULD alert: strih genlock CONFIRMED UNLOCKED"
#    (run it twice so the 2-pass confirm is satisfied; the dry-run uses a separate state file)

# 2. Dry-run against the LIVE fleet (inspect the real per-box verdict + decision):
scripts/genlock-lock-alert-watchdog.sh --dry-run

# 3. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/genlock-lock-alert-watchdog.service ~/.config/systemd/user/
cp systemd/genlock-lock-alert-watchdog.timer   ~/.config/systemd/user/
systemctl --user daemon-reload

# 4. Live-verify BEFORE enabling the timer:
#    a) with the fleet LOCKED -> a manual pass must report HEALTHY and take NO action:
systemctl --user start genlock-lock-alert-watchdog.service ; journalctl --user -u genlock-lock-alert-watchdog -n 60
#    b) force one unlock (stop dantesync on a box ~30 s so the indicator flips to UNLOCKED),
#       confirm two consecutive passes page EXACTLY ONCE with the correct box + verdict + reason,
#       then confirm the recovery line is logged (machine-channel) once dantesync is back.

# 5. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now genlock-lock-alert-watchdog.timer
systemctl --user list-timers | grep genlock-lock-alert-watchdog

# Disable later:
systemctl --user disable --now genlock-lock-alert-watchdog.timer
```

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `GENLOCK_LOCK_BOXES` | `obs_fleet_boxes genlock-lock` | `name\|ip` pairs to watch (overrides the fleet default) |
| `GENLOCK_LOCK_CONFIRM_THRESHOLD` | `2` | consecutive UNLOCKED/DEGRADED passes before paging |
| `GENLOCK_LOCK_ALERT_THROTTLE_PASSES` | `12` | passes (~1h) between repeat pages of the same verdict |
| `GENLOCK_LOCK_CURL_TIMEOUT` | `10` | `:8899` HTTP fetch timeout (s) |
| `GENLOCK_LOCK_FETCH_CMD` | (unset) | Tier-0 seam: `<cmd> <ip>`-prints-the-body replaces the live curl (dry-run against a fixture) |
| `GENLOCK_LOCK_DECIDE` | `scripts/genlock_lock_decision.py` | the pure decision module |
| `GENLOCK_LOCK_ALERT_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-genlock-lock-alert.state` | per-box confirm/throttle/latch state |
