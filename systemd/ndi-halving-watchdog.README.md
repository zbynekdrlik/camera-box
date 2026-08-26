# ndi-halving-watchdog (#1203) — install / live-verify / arm / enable (SHIPS DISABLED)

DEV1-side NDI per-connection rate-halving auto-heal. A sibling of the dev1 alert-watchdog family
(network-reach #1001 / frozen-input #1052 / bundle-state #732 / cadence #794 / asio-starve #1023) —
but with a CURE arm none of the siblings has, shipping GATED OFF. The units are COMMITTED but NOT
enabled; the cure arm is a SEPARATE, deliberately-off env gate. The supervisor installs + enables
after a live-verify. Detection makes NO rig-side change (it reads the stream OBS log over ssh from
dev1 and calls `airuleset.py notify`); only the ARMED cure touches the live receiver.

## What it detects, and the cure

The vendored DistroAV receiver can degrade a per-connection pull to ~HALF the sender's cadence —
live 2026-08-25: stream `NDI 2ME PGM` ran **15,0/s at a 30,0/s sender**, `recv_capture_v3` cap_avg
**65,9 ms** vs a healthy sibling's ~16 ms, starving the genlock FIFO (late_holds self-climbing,
depth 11 ≪ cap 69). A `systemctl restart camera-box` does NOT clear it; a receiver **reattach**
(`obs_phase2.py idle-receiver` → `--restore`, overlay keeps the latency pin) restored 30,0/s /
12,6 ms instantly.

- **The tap:** the stream (receiver) OBS log prints, per input, every ≥5.0 s
  `HH:MM:SS.mmm: [distroav] recv-timing #797 '<input>': n=<N> cap_avg=<X>ms …`. `n=` is PER-INTERVAL
  (reset-on-read), so the rate is measured **within one pass** from the last two lines
  (`n_curr / (ts_curr − ts_prev)`, both timestamps the lines' OWN prefixes — the #794 phantom-50
  avoidance), never a wall-clock divisor.
- **HALVED** = fps ≤ 0.6×expected **OR** cap_avg ≥ 2× the frame interval; **HEALTHY** = fps ≥
  0.85×expected AND cap_avg < 1.5× the interval; the band between = **BORDERLINE** (report-only).
  Confirmed over 2 consecutive passes before acting. The alert names whether a healthy SIBLING input
  exists this pass (per-connection vs box-wide context — context only, never a page gate).
- **Cure (gated):** on a CONFIRMED halving, if `NDI_HALVING_SELFHEAL=1` AND a per-input cooldown has
  elapsed, ONE reattach, then re-measure next pass. Cure disabled, or a cure that didn't take within
  the cooldown → PAGE (no reattach-spam). A one-shot recovery ping fires when the input reads HEALTHY
  again.

## Set NDI_HALVING_INPUTS to the live receiver input labels first

The default `NDI_HALVING_INPUTS` is `NDI 2ME PGM|30` (the live-confirmed leg). It is a `;`-list of
`<OBS input name>|<expected fps>` — extend it with other continuously-live receiver inputs (each with
its own expected fps) before enabling; an input that never emits a `recv-timing #797` line fires a
"tap broken" WARN after ~2 h. **Note on the cure for non-2ME-PGM inputs:** the default reattach
(`obs_phase2.py idle-receiver`) restores with `genlock_fifo: True` — correct for the genlocked
program feed, but a cure on a non-genlock-FIFO input would flip that setting on. If you extend
`NDI_HALVING_INPUTS` to such an input, either leave the cure arm OFF for it (alert-only is always
safe) or confirm the restore's genlock_fifo default is right for it first. Read the live labels +
their recv-timing lines:

```bash
sshpass -p newlevel ssh -o StrictHostKeyChecking=no newlevel@10.77.9.204 \
  'powershell -NoProfile -Command "gc (gci $env:APPDATA\obs-studio\logs\*.txt | sort LastWriteTime | select -last 1).FullName -Tail 400"' \
  | grep -oE "recv-timing #797 '[^']+'" | sort -u
```

## Live-verify from dev1 BEFORE enabling (a dry-run against the real stream log)

```bash
cd ~/devel/camera-box
scripts/ndi-halving-watchdog.sh --dry-run
# Expect a per-input `fps=… cap_avg=… -> HEALTHY/HALVED/BORDERLINE` line. A healthy 2ME PGM reads
# ~30 fps / low cap_avg -> HEALTHY and holds. Run twice ~5 s apart if the first pass reads UNKNOWN
# (a single recv-timing line is not yet measurable). --dry-run never cures and never pages.
```

Offline smoke-test with a stub (no rig): set `NDI_HALVING_PROBE_CMD` to a command that prints raw
OBS-log text for `<receiver_ip>` (all inputs' recv-timing lines in one output), and
`NDI_HALVING_CURE_CMD` to a no-op `<ip> <input>` command.

## Enable the ALERT (dev1, user timer) — cure arm stays OFF

The repo `systemd/` dir is NOT in systemd's unit search path, so copy the units in first:

```bash
mkdir -p ~/.config/systemd/user
cp ~/devel/camera-box/systemd/ndi-halving-watchdog.service ~/.config/systemd/user/
cp ~/devel/camera-box/systemd/ndi-halving-watchdog.timer   ~/.config/systemd/user/
# persist the live NDI_HALVING_INPUTS (+ any threshold override) via a drop-in:
#   systemctl --user edit ndi-halving-watchdog.service
#     [Service]
#     Environment=NDI_HALVING_INPUTS=NDI 2ME PGM|30;NDI cam1|60
systemctl --user daemon-reload
systemctl --user enable --now ndi-halving-watchdog.timer
systemctl --user list-timers | grep ndi-halving
```

## Arm the CURE (a DELIBERATE, supervised step — default OFF)

The cure REATTACHES a live receiver (idle clears the NDI name for one render tick, then restores it).
Arm it only after the alert has proven the detection on the live rig, and only with the stream OBS
WebSocket password set (the cure needs it):

```bash
systemctl --user edit ndi-halving-watchdog.service
  [Service]
  Environment=NDI_HALVING_SELFHEAL=1
  Environment=NDI_HALVING_OBS_WS_PW=<stream OBS WebSocket password>
systemctl --user daemon-reload
```

**Risk note:** the reattach is a two-step idle→restore; if the box goes unreachable BETWEEN the two
steps, the input can be left with an empty NDI name (a stopped receiver thread — see
`.claude/rules/ndi-name-recovery.md`). The watchdog retries the restore and logs LOUD on a
persistent restore failure. This is why the cure ships OFF and is armed only deliberately.

## Config knobs (all env-overridable)

| Env | Default | Meaning |
|---|---|---|
| `NDI_HALVING_RECEIVER` | `stream\|10.77.9.204` | box whose OBS log carries recv-timing #797 + the cure target |
| `NDI_HALVING_SENDER` | `strih` | 2ME PGM producer (no-double-page guard only) |
| `NDI_HALVING_INPUTS` | `NDI 2ME PGM\|30` | `;`-list of `<input>\|<expected_fps>` (SET to the live set) |
| `NDI_HALVING_SELFHEAL` | `0` | arm the reattach cure (1); default OFF = alert-only |
| `NDI_HALVING_COOLDOWN_S` | `600` | per-input cure cooldown (one reattach per window) |
| `NDI_HALVING_OBS_WS_PW` | `$OBS_PASSWORD` | stream OBS WebSocket password (needed only when the cure is armed; an armed empty password warns loudly each pass) |
| `NDI_HALVING_OBS_WS_CALL_TIMEOUT_S` | `40` | per obs_phase2 WS call cap (idle + 2 restores must fit inside the unit's TimeoutStartSec) |
| `NDI_HALVING_STALE_AFTER_S` | `12.0` | a source whose newest line sits this far behind the log's newest line → UNKNOWN (stopped emitting) |
| `NDI_HALVING_RATIO` | `0.6` | HALVED if fps ≤ ratio×expected |
| `NDI_HALVING_CAP_MULT` | `2.0` | HALVED if cap_avg ≥ mult×(1000/expected) ms |
| `NDI_HALVING_HEALTHY_RATIO` | `0.85` | HEALTHY floor (fps ≥ ratio×expected) |
| `NDI_HALVING_HEALTHY_CAP_MULT` | `1.5` | HEALTHY cap ceiling (× the frame interval) |
| `NDI_HALVING_MAX_WINDOW_S` | `15.0` | a pair spanning more → UNKNOWN (freeze/gap straddle) |
| `NDI_HALVING_CONFIRM_THRESHOLD` | `2` | consecutive HALVED passes before acting |
| `NDI_HALVING_ALERT_THROTTLE_PASSES` | `6` | re-alert cadence (~30 min at the 5-min timer) |
| `NDI_HALVING_TAP_BROKEN_THRESHOLD` | `24` | consecutive blind passes before a "tap broken" WARN (~2 h) |
| `NDI_HALVING_PROBE_CMD` | (unset) | override the ssh read (dry-run/stub); run ONCE per pass with `<receiver_ip>`, stdout = raw OBS-log text |
| `NDI_HALVING_CURE_CMD` | (unset) | override the reattach (test stub); run with `<receiver_ip> <input>`, exit 0 = attempted |
