# rig-restore-watchdog — install note (#281 Fix#3)

The rig auto-restore watchdog (`scripts/rig-restore-watchdog.sh`) is the safety net for #281:
dispatched workers die mid-rig-task and leave the rig stranded in a TEST state (prod `camera-box`
down while a manual `/tmp` probe holds the capture device, OBS program on a probe scene, burns
left on). The watchdog DETECTS a confirmed stranded rig, AUTO-RECOVERS prod (restart `camera-box`,
`obs_phase2.py teardown` to the prod scene + burns off), and ALWAYS fires a Discord alert. It runs
on **dev1** from a `systemd --user` timer, so recovery happens even when no Claude session is alive.

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo. The previous #266
auto-watchdog was removed for false positives, so before this one ever runs unattended the
**SUPERVISOR** must install it, **live-verify** it (simulate a stranded state, confirm
detect → restore → alert with NO false positive against a real running E2E heartbeat), and only
then enable the timer. Do **not** enable it as part of merging the PR.

## Conservative gates (why it won't repeat #266)

- A **fresh heartbeat** (`scripts/lib/rig-heartbeat.sh`, written + refreshed by a live
  `recording-e2e.sh` / `rig-mode.sh test`) means a legit E2E is running → it **never acts**.
- It acts only on a **clear stranded signal**: cam-box down, a stale probe process, or OBS program
  on a known TEST scene (default `PHASE2-PROBE`; override with `RIG_KNOWN_TEST_SCENES`).
- It requires **2 consecutive confirmations** (`RIG_CONFIRM_THRESHOLD`, default 2) before acting —
  one stranded read is observe-only.
- An unreachable cam box / unreadable OBS is **not** treated as stranded.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — observe + decide + LOG only, NEVER restore/alert:
scripts/rig-restore-watchdog.sh --dry-run        # inspect the per-node observations + decision

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/rig-restore-watchdog.service ~/.config/systemd/user/
cp systemd/rig-restore-watchdog.timer   ~/.config/systemd/user/
# Set OBS WS password (NOT committed) if strih requires it:
#   mkdir -p ~/.config/environment.d
#   printf 'OBS_WS_PASSWORD=...\n' > ~/.config/environment.d/rig-restore-watchdog.conf
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer:
#    a) with a REAL E2E running (fresh heartbeat) -> a manual pass must NOT act:
systemctl --user start rig-restore-watchdog.service ; journalctl --user -u rig-restore-watchdog -n 50
#    b) simulate a stranded state (e.g. stop camera-box on one cam OR leave OBS on PHASE2-PROBE,
#       with NO heartbeat) -> two consecutive passes must restore prod + fire the Discord alert.

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now rig-restore-watchdog.timer
systemctl --user list-timers | grep rig-restore-watchdog

# Disable later:
systemctl --user disable --now rig-restore-watchdog.timer
```

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `RIG_CONFIRM_THRESHOLD` | `2` | consecutive confirmations before acting |
| `RIG_HEARTBEAT_STALE_SEC` | `600` | age beyond which a heartbeat is "stale" (no longer live proof) |
| `RIG_KNOWN_TEST_SCENES` | `PHASE2-PROBE` | space-separated program scenes that prove a TEST state |
| `CAM_PW` | `newlevel` | dev-rig root pw (same default as `recording-e2e.sh`) |
| `OBS_WS_PASSWORD` | (empty) | strih OBS WS pw; stream is no-auth |
| `RIG_WATCHDOG_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-rig-watchdog.state` | confirm-counter persistence |
