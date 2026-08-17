# cadence-alert-watchdog (#794) — install / live-verify / enable (SHIPS DISABLED)

DEV1-side non-60 source-cadence alert. Fourth sibling of the dev1 alert-watchdog family
(network-reach #1001 / frozen-input #1052 / bundle-state #732). The units are COMMITTED but NOT
enabled — the supervisor installs + enables after a live-verify. This watchdog makes **no** rig-side
change: it only reads strih's OBS log over ssh from dev1 and calls `airuleset.py notify`.

## What it detects (and its known blind spot)

A camera genuinely delivering a **non-60 fps** rate over NDI advances strih's genlock-fifo
`received=` counter at that rate (50/s for a 50 fps camera, 43/s for 43). The watchdog measures each
watched source's rate from two sampled audit lines' OWN timestamps — never a wall-clock divisor (the
issue-797 "phantom 50.1 fps" avoidance) — and pages when it sits sustained outside 60 ± 3 fps.

**Blind spot (a separate follow-up):** a grabber that upconverts 50→60 by DUPLICATION delivers a
padded genuine 60 NDI frames/s, so `received=` reads a clean 60 here. That duplication-masked "hard
layer" needs per-frame content hashing (pixel access) — filed separately.

## Set CADENCE_SOURCES to the LIVE strih camera labels first

The default `CADENCE_SOURCES` is a placeholder (`NDI cam1;…;NDI cam7`). Before enabling, set it to
the actual strih input labels of the **currently-active** cameras (the `genlock-fifo audit '<name>':`
quoted names in strih's OBS log) via a drop-in — a listed source that never emits an audit line will
fire a "tap broken" WARN after ~2 h. Read the live labels:

```bash
sshpass -p newlevel ssh -o StrictHostKeyChecking=no newlevel@10.77.9.202 \
  'powershell -NoProfile -Command "gc (gci $env:APPDATA\obs-studio\logs\*.txt | sort LastWriteTime | select -last 1).FullName -Tail 400"' \
  | grep -oE "genlock-fifo audit '[^']+'" | sort -u
```

## Live-verify from dev1 BEFORE enabling (a dry-run against the real strih log)

```bash
cd ~/devel/camera-box
CADENCE_SOURCES='NDI cam1;NDI cam2;…' scripts/cadence-alert-watchdog.sh --dry-run
# Expect: pass 1 seeds every source (UNKNOWN, no prior). Run it again after ≥60 s to see a real
# measured fps per source (`fps=… win=… -> OK`). A healthy fleet reads ~60 and holds OK.
```

Offline smoke-test with a stub (no rig): set `CADENCE_PROBE_CMD` to a command that prints raw OBS
log text for `<box_ip> <source>` (see the worktree's issue-794 scratchpad stub for the shape).

## Enable (dev1, user timer)

```bash
mkdir -p ~/.config/systemd/user
cp ~/devel/camera-box/systemd/cadence-alert-watchdog.service ~/.config/systemd/user/
cp ~/devel/camera-box/systemd/cadence-alert-watchdog.timer   ~/.config/systemd/user/
# persist the live CADENCE_SOURCES (+ any threshold override) as a drop-in EnvironmentFile or a
# Service Environment= drop-in, so the timer's oneshot picks it up:
#   systemctl --user edit cadence-alert-watchdog.service
#     [Service]
#     Environment=CADENCE_SOURCES=NDI cam1;NDI cam2;…
systemctl --user daemon-reload
systemctl --user enable --now cadence-alert-watchdog.timer
systemctl --user list-timers | grep cadence
```

## Config knobs (all env-overridable)

| Env | Default | Meaning |
|---|---|---|
| `CADENCE_BOX` | `strih\|10.77.9.202` | box whose OBS log carries the camera `received=` counters |
| `CADENCE_SOURCES` | 7-camera placeholder | `;`-list of watched strih input labels (SET to the live active set) |
| `CADENCE_EXPECTED_FPS` | `60` | the healthy delivered rate |
| `CADENCE_TOLERANCE_FPS` | `3` | WRONG outside [57,63]; 59.94 NTSC is in-band |
| `CADENCE_MIN_WINDOW_S` | `60` | windows shorter than this → UNKNOWN (never a noisy page) |
| `CADENCE_ALERT_CONFIRM_THRESHOLD` | `2` | consecutive WRONG passes before paging |
| `CADENCE_ALERT_THROTTLE_PASSES` | `6` | re-alert cadence (~30 min at the 5-min timer) |
| `CADENCE_TAP_BROKEN_THRESHOLD` | `24` | consecutive blind passes before a "tap broken" WARN (~2 h) |
| `CADENCE_PROBE_CMD` | (unset) | override the ssh read (dry-run/stub); run with `<box_ip> <source>`, stdout = raw log text |
