# audio-lag-alert-watchdog — install note (#1226)

The dev1-side alert watchdog (`scripts/audio-lag-alert-watchdog.sh`) closes the detection gap behind
the **2026-08-30 nedeľná služba** incident: stream OBS's audio subsystem began lagging realtime
PRECISELY at StartStream (09:39) and lost ~24–27 s/min; `audio-telemetry #800 '<src>': ts_lag_ms=N`
(`vendor/obs-studio/libobs/obs-audio.c:698`) grew to **1 672 741 ms (27,9 min)** and SCREAMED into
the OBS log **the whole hour** — but nothing off the box read it, so the YouTube stream's A/V
desynced for a whole service before a viewer noticed. `obs-liveness` (#391) watches RENDER not the
audio timeline; the av-sync dock is structurally blind during a service (program = real cameras, no
QR); `asio-starve` (#1023) measures per-source starvation, not the global audio-tick lag.

It reads the **`audio_ts_lag_ms` facet** `bundle_state_gather` (#1226) now exposes on each box's
`:8899/bundle-state.json` (the MAX per-source lag from the newest `#800` line, parsed from the SAME
#1222 bounded head+tail read — no second log scan), polling both boxes from a **dev1 systemd --user
timer** with `curl` (the ops-SKILL-mandated method — an MCP-side `Invoke-WebRequest` hangs even when
the server logs a prompt 200). It is the audio-timeline sibling of `bundle-state-alert-watchdog`
(#732) and `network-reach-alert-watchdog` (#1001).

## DETECTION ONLY — no auto-action (unlike bundle-state's auto-restart)

The only observed cure was a **PC reboot of a live prod box** — a genuinely destructive owner-call
(`no-destructive-remote-actions.md`), and the preventive pre-service reboot is an **owner decision on
the ticket**, not automation. So this watchdog is **alert-only**: on a confirmed lag it pages
(throttled), and **recovery is log-only / machine-channel** (never a phone ping —
`.claude/rules/watchdog-notify-dedup.md` #1206; the alert carries a stable `--dedup-key
audio-lag-<box>` so a growing lag EDITS the one card instead of re-pinging).

## Separation of concerns — it never false-pages, and never double-pages a down box

The **only** page condition is a SUCCESSFULLY-FETCHED positive lag reading, so:

- A box whose `:8899` is not fetchable this pass (box down, or `:8899` down) classifies **SKIP** —
  deferred to `bundle-state-alert-watchdog` (#732) / `network-reach-alert-watchdog` (#1001). No audio
  page for a down box, no duplicate.
- A box that is up but has **no `audio_ts_lag_ms` facet yet** (a stock OBS, or no `#800` line in the
  log tail yet) classifies **UNKNOWN** — no reading to judge, no page.
- Because a page requires a fetched positive reading, a **dev1-side path outage** makes every fetch
  fail → SKIP → no page. That is why this watchdog needs **no** reference-anchor/outage guard (unlike
  bundle-state #732, which restarts tasks and pages on a DOWN box and therefore does need one).

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo / this PR. Before it
runs unattended, the **SUPERVISOR** installs it, live-verifies it (below), and only then enables the
timer. No box-side change is made by this ticket.

## Conservative gates (why it won't thrash or spam)

- Requires **2 consecutive confirmations** (`AUDIO_LAG_CONFIRM_THRESHOLD`, default 2) before it
  alerts — one blipped reading is observe-only. A HEALTHY read resets the counter; a SKIP/UNKNOWN
  pass HOLDS the counter (an unmeasured pass neither advances nor resets it).
- The page threshold is `AUDIO_LAG_THRESHOLD_MS` (default **5000 ms**) — a wide margin above the
  healthy ~107–132 ms baseline and well below the point a viewer notices, so it catches the growth
  EARLY. Repeat alerts are throttled to once every `AUDIO_LAG_ALERT_THROTTLE_PASSES` passes
  (default 12 ≈ 1h) while the same box stays lagging.
- A recovery ("back to normal") line is logged once (machine-channel) when a box we paged for
  returns to a healthy lag.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — fetch + decide + LOG only, NEVER alert:
scripts/audio-lag-alert-watchdog.sh --dry-run           # inspect the per-box verdict + decision

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/audio-lag-alert-watchdog.service ~/.config/systemd/user/
cp systemd/audio-lag-alert-watchdog.timer   ~/.config/systemd/user/
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer:
#    a) with both boxes healthy -> a manual pass must report HEALTHY and take NO action:
systemctl --user start audio-lag-alert-watchdog.service ; journalctl --user -u audio-lag-alert-watchdog -n 50
#    b) simulate a lag: temporarily point AUDIO_LAG_THRESHOLD_MS below a box's live lag (or serve a
#       crafted bundle-state), confirm two consecutive passes page with the correct box + lag + src,
#       then confirm the recovery line is logged (machine-channel) once it clears.

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now audio-lag-alert-watchdog.timer
systemctl --user list-timers | grep audio-lag-alert-watchdog

# Disable later:
systemctl --user disable --now audio-lag-alert-watchdog.timer
```

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `AUDIO_LAG_THRESHOLD_MS` | `5000` | audio-timeline lag (ms) above which a box is LAGGING |
| `AUDIO_LAG_STALE_THRESHOLD_S` | `180` | #1231: telemetry age (s) behind the OBS log head above which it is STALE (surfaced distinctly, never paged). Note the box-side per-source stale filter is a hardcoded 180 s (`bundle_state_gather.AUDIO_TS_LAG_STALE_AFTER_S`): raising this dev1 value above 180 only ever narrows to UNKNOWN, never widens the fresh window — the safe direction (never a page) |
| `AUDIO_LAG_CONFIRM_THRESHOLD` | `2` | consecutive LAGGING readings before paging |
| `AUDIO_LAG_ALERT_THROTTLE_PASSES` | `12` | passes between repeat alerts (≈1h at the 5-min cadence) |
| `AUDIO_LAG_BOXES` | `strih\|10.77.9.202 stream\|10.77.9.204` | `name\|ip` pairs to watch |
| `AUDIO_LAG_CURL_TIMEOUT` | `10` | `:8899` HTTP fetch timeout (s) |
| `AUDIO_LAG_ALERT_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-audio-lag-alert.state` | per-box confirm/throttle/recovery state |

## What this does NOT do

- It makes **no box-side change** and takes **no auto-action** — the cure (an OBS/box reboot) is an
  owner call; this watchdog only detects and alerts.
- It does not diagnose **why** a box degrades into an audio lag (the 2026-08-30 root mechanism —
  MM-timer resolution / paging / a driver over a 2,6-day uptime — was not log-provable). It detects
  the *symptom* (a growing `ts_lag_ms`) early, so the operator can act before a viewer hears it. The
  preventive **pre-service reboot** is tracked as an owner decision on #1226.
