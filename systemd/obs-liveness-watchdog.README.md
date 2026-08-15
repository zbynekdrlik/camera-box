# obs-liveness-watchdog — install note (#391)

The broadcast-OBS liveness watchdog (`scripts/obs-liveness-watchdog.sh`) is the safety net for
#391: stream OBS (10.77.9.204) was hung "(Not Responding)" for **~25 HOURS** (obs64 pegged ~168%
CPU, `Responding=False`, 16.0% frames-missed-due-to-render-lag) and **nothing detected it** — the
user found it manually a day later. The watchdog polls both broadcast OBS boxes (strih
10.77.9.202, stream 10.77.9.204) over OBS WebSocket `GetStats` on a **dev1 systemd --user timer**,
runs the strict `camera_box::obs_watchdog::classify` verdict, and — once a wedge is **confirmed**
over 2 consecutive passes — fires a Discord alert immediately. It is the Windows-OBS sibling of the
#281/#350 `rig-restore-watchdog`.

## Auto-recover design decision — DETECT + ALERT automatically, RECOVERY is agent-driven

The win-* MCP is **agent-only** (a systemd --user timer has no agent session to drive it) — and
even though #701 proved plain scp/ssh actually reaches strih/stream with the `targets.md` creds, a
headless ssh-based recovery for THIS timer was never built. So a dev1 timer can fully **DETECT** a
wedge (OBS WebSocket is network-reachable, no ssh/MCP needed for `GetStats`) but **cannot itself
force-kill or relaunch** a wedged `obs64.exe` process today — every other Windows recovery action
in this repo already goes through an agent driving the win-* MCP
(`scripts/launch-obs-genlock.sh` is itself a PURE PLANNER that only
PRINTS the PowerShell program to paste into the box's MCP `Shell` — see that script's own header;
driving/verifying a GUI relaunch is exactly what the MCP is for, ssh reachability alone wouldn't
replace that).

So this watchdog:
1. **Detects** a confirmed wedge automatically, unattended, from the dev1 timer.
2. **Alerts** the owner via Discord immediately, embedding the exact ready-to-run recovery
   command: `bash scripts/launch-obs-genlock.sh --box <box> --force` (paste the printed PowerShell
   program into the box's win-strih / win-stream-snv MCP Shell).
3. **Recovery is agent-mediated from THIS dev1 timer** — consistent with 100% of existing
   Windows-recovery precedent in this repo. **#411 additionally ships a Windows-LOCAL unattended
   self-heal** (`scripts/obs-self-heal-install.sh` + `src/obs_self_heal.rs`) — a per-box Task
   Scheduler job that runs the SAME `obs_watchdog::classify` verdict locally (via
   `obs-watchdog-gate.exe` against local process signals) and force-kills + relaunches obs64
   itself when a wedge is confirmed, closing the exact overnight/no-agent-watching gap this dev1
   timer alone cannot close. It sequences around the existing AHK auto-respawn watcher on strih
   (see `.claude/skills/obs-ops` "AHK on strih") by construction — AHK is stopped before obs64 is
   ever touched and restarted only after the relaunch is verified, so the two mechanisms can never
   race or double-launch obs64. Ships DISABLED; see `scripts/obs-self-heal-install.sh --help` for
   the supervisor install + live-verify procedure. This dev1 timer's Discord alert stays in place
   regardless — a human is still notified even when the local self-heal already recovered the box.

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo. Before this watchdog
ever runs unattended, the **SUPERVISOR** must install it, **live-verify** it (a genuine wedge
alerts, a healthy box never false-positives), and only then enable the timer. Do **not** enable it
as part of merging the PR.

## Conservative gates (why it won't spam false alerts)

- Requires **2 consecutive confirmations** (`OBS_WATCHDOG_CONFIRM_THRESHOLD`, default 2) before
  alerting — one WS hiccup / transient GetStats blip is observe-only, never an alert.
- A healthy read on any pass **resets** that box's confirm counter to 0.
- Once confirmed, repeat alerts for the SAME persisting condition are throttled to once every
  `OBS_WATCHDOG_ALERT_THROTTLE_PASSES` passes (default 10, ≈20 min at the 2-min timer cadence) — a
  changed condition (e.g. escalating from WEDGED-RENDER-LAG to OBS-COUNT-WRONG) always re-alerts
  immediately regardless of the throttle.
- A WS measurement failure for a box is reported as `WS-DEAD` (a real detection signal), not
  silently swallowed — but a *probe/binary* failure (no verdict lines produced at all) is logged
  and skipped for that pass rather than guessed at.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — measure + decide + LOG only, NEVER alert:
scripts/obs-liveness-watchdog.sh --dry-run        # inspect the per-box verdict + decision

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/obs-liveness-watchdog.service ~/.config/systemd/user/
cp systemd/obs-liveness-watchdog.timer   ~/.config/systemd/user/
# Set strih's OBS WS password (NOT committed) if it requires one; stream is no-auth:
#   mkdir -p ~/.config/environment.d
#   printf 'OBS_PASSWORD_STRIH=...\n' > ~/.config/environment.d/obs-liveness-watchdog.conf
# Point at the downloaded probe-tools-linux-amd64 CI artifact (carries obs-watchdog-gate):
#   printf 'PROBE_BIN_DIR=/path/to/probe-tools-linux-amd64\n' >> ~/.config/environment.d/obs-liveness-watchdog.conf
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer:
#    a) with BOTH boxes healthy -> a manual pass must NOT alert:
systemctl --user start obs-liveness-watchdog.service ; journalctl --user -u obs-liveness-watchdog -n 50
#    b) simulate a wedge (e.g. force-kill obs64 without relaunching, or pass --process-state via a
#       manual scripts/obs-liveness-probe.py invocation with responding:false/cpu:170) -> two
#       consecutive passes must alert with the correct box + verdict + recovery command.

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now obs-liveness-watchdog.timer
systemctl --user list-timers | grep obs-liveness-watchdog

# Disable later:
systemctl --user disable --now obs-liveness-watchdog.timer
```

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `OBS_WATCHDOG_CONFIRM_THRESHOLD` | `2` | consecutive wedge readings before alerting |
| `OBS_WATCHDOG_ALERT_THROTTLE_PASSES` | `10` | passes between repeat alerts for the same condition |
| `OBS_WATCHDOG_WINDOW_S` | `4` | GetStats delta measurement window (seconds) per pass |
| `STRIH_HOST` / `STREAM_HOST` | `10.77.9.202` / `10.77.9.204` | broadcast box addresses |
| `STRIH_TARGET_FPS` / `STREAM_TARGET_FPS` | `60` / `30` | final mixed 60+30 topology targets |
| `OBS_PASSWORD_STRIH` | (empty) | strih OBS WS pw; stream is no-auth |
| `PROBE_BIN_DIR` | (unset) | dir containing the `obs-watchdog-gate` CI artifact binary |
| `OBS_WATCHDOG_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-obs-watchdog.state` | per-box confirm/throttle state |

## Root cause of the 25h wedge — not reproducible from available telemetry

No DXGI/`DEVICE_REMOVED`/TDR signature was found (ruling out the #89 GPU-removed case). Windows
Event Log (`Application`, event ID 1002 "stopped interacting with Windows") shows NO entry for the
actual incident window — the OS-level hang detector did not fire/log for this wedge, which is
itself evidence that relying on OS-level signals alone is insufficient; only application-level
telemetry (OBS WebSocket `GetStats` + `Responding`/CPU) caught it. If this recurs, the next
diagnosable step is capturing obs64's log tail + full process/thread state via the win-* MCP
**before** force-killing during the agent-driven recovery — see `.claude/skills/obs-ops` "Wedged
OBS" recovery section.

## #935 — render-loop stall detection now works on a WS-only pass (`render_advanced`)

The 2026-08-02 strih incident (issue 935) exposed a hole: a full graphics render-loop stall
(`renderTotalFrames` delta 0 over 3 s) while `GetStats activeFps` still reported the configured
30.0 and the WS thread answered. The WS-only probe used to fill `active_fps` from the lying
`activeFps` gauge and computed `render_skipped_frac = 0.0` on a frozen loop, so `classify()`
returned HEALTHY — enabling this watchdog as-is would NOT have paged. Fixed: the probe now also
emits `render_advanced` (did `renderTotalFrames` advance over the window), and `classify()` returns
FPS-ZERO when it is `Some(false)`, on the plain dev1 WS-only pass with no process/MCP signals. This
watchdog still SHIPS DISABLED — the fix makes it CAPABLE of catching that class; the supervisor must
still install + live-verify + enable it per the procedure above. (It does NOT cover a frozen strih
cambox INPUT while strih keeps compositing — that is a separate watchdog scope, tracked separately.)
