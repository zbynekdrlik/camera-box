# obs-session-watchdog — install note (#979)

The obs64/AHK Windows-session-visibility watchdog (`scripts/obs-session-watchdog.sh`) is the
CONTINUOUS sibling of #977's per-PR E2E gate preflight, and closes issue 958's real gap: a
session-0 `obs64` (launched via ssh+`Invoke-CimMethod`) is fully healthy on OBS WebSocket, NDI, and
the OBS log — completely invisible to the operator on the console — yet #977's gate only fires on
a push. The real incident sat like this for **~3.5 hours** before the user found it manually. This
watchdog polls both broadcast boxes (strih 10.77.9.202, stream 10.77.9.204) over `win_ssh_run`
(`scripts/lib/win-ssh-exec.sh`, #703) on a **dev1 systemd --user timer**, reusing the SAME
session-visibility probe #977/#978 use (`scripts/lib/obs-session-visibility.sh`) and the SAME
`#391 obs_watchdog_confirm`/`obs_watchdog_alert_throttle` decision logic
(`scripts/lib/obs-watchdog-decision.sh`) — no second/third mechanism anywhere in this stack.

## Auto-recover design decision — DETECT + ALERT automatically, RECOVERY is agent-driven

Same precedent as `scripts/obs-liveness-watchdog.sh` (#391) and `scripts/imag-obs-alert-
watchdog.sh` (#882): the win-* MCP is agent-only, so a dev1 systemd timer cannot itself drive a GUI
relaunch. This watchdog **detects** a confirmed invisibility automatically and **alerts** the owner
via Discord, embedding the exact ready-to-run recovery command:
`bash scripts/launch-obs-genlock.sh --box <box> --force` (paste the printed PowerShell program into
the box's win-strih / win-stream-snv MCP Shell — the SAME wrapper #978 hardened to self-verify
session visibility on every future launch).

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo. Before this watchdog
ever runs unattended, the **SUPERVISOR** must install it, **live-verify** it (a genuine session-0
obs64/AHK alerts, a healthy box never false-positives), and only then enable the timer. Do **not**
enable it as part of merging the PR.

## Conservative gates (why it won't spam false alerts)

- Requires **2 consecutive confirmations** (`OBS_SESSION_WATCHDOG_CONFIRM_THRESHOLD`, default 2)
  before alerting — one transient ssh/probe hiccup is observe-only, never an alert.
- A visible read on any pass **resets** that box's confirm counter to 0.
- Once confirmed, repeat alerts for the SAME persisting condition are throttled to once every
  `OBS_SESSION_WATCHDOG_ALERT_THROTTLE_PASSES` passes (default 10, ≈50 min at the 5-min timer
  cadence) — a changed condition (e.g. escalating from a session-0 obs64 to a missing AHK) always
  re-alerts immediately regardless of the throttle.
- An **empty ssh probe (connectivity failure) is logged and skipped for that pass, never treated
  as a false INVISIBLE alert** — the fleet's own `[0/8]` reachability preflight is the authority
  for connectivity, not this watchdog (same explicit precedent as #882).

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — measure + decide + LOG only, NEVER alert:
scripts/obs-session-watchdog.sh --dry-run        # inspect the per-box probe + decision

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/obs-session-watchdog.service ~/.config/systemd/user/
cp systemd/obs-session-watchdog.timer   ~/.config/systemd/user/
# strih/stream ssh creds default to targets.md's "SSH: newlevel/newlevel"; override if needed:
#   mkdir -p ~/.config/environment.d
#   printf 'STRIH_PW=...\nSTREAM_PW=...\n' > ~/.config/environment.d/obs-session-watchdog.conf
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer:
#    a) with BOTH boxes healthy (session=1, visible window) -> a manual pass must NOT alert:
systemctl --user start obs-session-watchdog.service ; journalctl --user -u obs-session-watchdog -n 50
#    b) simulate a genuine session-0 relaunch on ONE box (e.g. via the documented ssh+CIM recipe,
#       agent-driven, then restore via launch-obs-genlock.sh --force afterward) -> two consecutive
#       passes must alert with the correct box + reason + recovery command.

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now obs-session-watchdog.timer
systemctl --user list-timers | grep obs-session-watchdog

# Disable later:
systemctl --user disable --now obs-session-watchdog.timer
```

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `OBS_SESSION_WATCHDOG_CONFIRM_THRESHOLD` | `2` | consecutive invisible readings before alerting |
| `OBS_SESSION_WATCHDOG_ALERT_THROTTLE_PASSES` | `10` | passes between repeat alerts for the same condition |
| `STRIH_HOST` / `STREAM_HOST` | `10.77.9.202` / `10.77.9.204` | broadcast box addresses |
| `STRIH_USER` / `STRIH_PW` / `STREAM_USER` / `STREAM_PW` | `newlevel` / `newlevel` | ssh creds (targets.md) |
| `OBS_SESSION_WATCHDOG_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-obs-session-watchdog.state` | per-box confirm/throttle state — deliberately DIFFERENT from #391's own state file, since both key on the same box names |
