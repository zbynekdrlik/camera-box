# measurement-audio-alert-watchdog — install note (#1310)

The dev1-side alert watchdog (`scripts/measurement-audio-alert-watchdog.sh`) closes the detection gap
behind the mbc **measurement-audio** chain reading DIGITAL SILENCE between E2E runs. The chain — cam2
HDMI monitor speaker plays the QPSK marker → measurement mic → mbc Ableton Live 12 on `10.77.7.232`
→ Dante Virtual Soundcard → stream OBS ASIO input **`mbc`** — is the instrument the whole A/V-sync
leg reads. After a production it can go silent (mic switched off, Ableton mbc channel muted, Dante
route dropped) and **nothing pages it**: the only mbc-silence check today is the in-RUN #748 preflight
(`recording-e2e.sh [4b2/8]`), so a silent chain is invisible until the next full ~300 s E2E burns a
cycle discovering it — the release E2E **34764817477** abort (`max_volume -91.0 dB`, `n=120` samples
flowing but all zeros) and the 2026-07-12 „mutnutý mikrofón prežil týždeň nepovšimnutý" incident.

Owner directive (2026-09-13): *„stale nevidim qrkod a zvuk z cam2"* + the #1308 class *„aj ntp aj
ostatne veci bez ktorych nevie produkcia bezat spravne musi notifikovat … byt o tom dokolecka
notifikovany"*. So this is a **production-critical** watchdog: while a fault persists it **re-pings
repeatedly** (time-bucketed dedup key). It is the measurement-audio sibling of `audio-lag-alert-watchdog`
(#1226), `dantesync-clock-alert-watchdog` (#1307), and `network-reach-alert-watchdog` (#1001), and a
member of the production-critical class tracked under **#1308**.

## How it reads the level — no recording

Every 60 s (a dev1 `--user` timer) it reads the `mbc` input's **peak level** off stream OBS via the
**obs-websocket `InputVolumeMeters`** high-volume event (`scripts/measurement_audio_meter_probe.py` —
subscribe, sample ~2 s, take the max peak, convert to dBFS). **No StartRecord, no disk, no rig
mutation** — safe to run continuously even near a live show. The VERDICT is decided by the PURE
`scripts/measurement_audio_decision.py` (pytest Tier-0, the #1199 python-mirror pattern). The SILENCE
bar is the **same `-60` dB** the #748 preflight uses — sourced from
`scripts/lib/audio-presence-preflight.sh` (`audio_preflight_default_threshold_db`), never retyped.

## What it pages on

| Verdict | Meaning | Action |
|---|---|---|
| `SILENT` | reachable, `mbc` meter present, peak_db < -60 dB | **page** after a 2-pass confirm (production-critical time-bucketed re-ping); names the checklist |
| `PRESENT` | reachable, `mbc` meter present, peak_db ≥ -60 dB | healthy; clears the latch, recovery machine-channel only |
| `SKIP` | stream OBS `:4455` not reachable (WS connect failed) | defer to #1001 / #732; **never** our page |
| `UNKNOWN` | reachable but `mbc` never appeared in the meter stream (renamed/removed input, or InputVolumeMeters unavailable) | held, no page |

The paged card names the checklist in order: (1) is the measurement mic on at the cam2 monitor
speaker? (2) is the mbc channel in Ableton on `10.77.7.232` **UNMUTED**? (3) is the Dante routing from
mbc → DVS → stream OBS intact? (`targets.md` mbc row has the checklist.)

## TEST-premise — EVENT-gated like splitter-port (#1290)

The QPSK marker only sounds in **TEST** mode, so in **EVENT/production** a silent mbc is EXPECTED. The
watchdog computes the rig mode once per pass (`rig-mode-state.sh`'s cam2 painter probe →
EVENT / TEST / UNKNOWN) and in **EVENT** skips the whole check (no page) and clears any latch. **TEST**
or **UNKNOWN** → proceed (fail-safe: an unreadable mode never silences a real TEST-mode fault). This is
the rig-mode gate; it is ORTHOGONAL to the fault-criticality axis (a silent instrument is
production-critical even though it's only measurable in TEST).

## Production-critical time-bucketed re-ping (owner ruling — the one deviation from #1206)

Every page uses `airuleset.py notify --dedup-key`, but the key is **time-bucketed** via the ONE shared
helper `watchdog_notify_key`: `measurement-audio-stream-<floor(now/REPING_INTERVAL_S)>`
(`REPING_INTERVAL_S` default **600 s**, floored at 60 s). Within one bucket an identical state **edits**
the card (no re-ping); every new bucket is a **fresh ping** while the fault persists. The deliberate
exception to `.claude/rules/watchdog-notify-dedup.md`'s one-ping-per-incident rule, for the
production-critical class only (documented there + allowlisted in
`tests/python/test_notify_dedup_key_sweep_1206.py`). **Recovery is machine-channel / log-only.**

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo / this PR. Before it runs
unattended, the **SUPERVISOR** installs it, live-verifies it (below), and only then enables the timer.
No box-side change is made by this ticket.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — probe + decide + LOG only, NEVER alert (needs the rig reachable):
scripts/measurement-audio-alert-watchdog.sh --dry-run        # inspect the verdict + decision

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/measurement-audio-alert-watchdog.service ~/.config/systemd/user/
cp systemd/measurement-audio-alert-watchdog.timer   ~/.config/systemd/user/
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer (the mute → page → unmute → recovery loop):
#    a) with the marker AUDIBLE -> a manual pass reports PRESENT, no page:
systemctl --user start measurement-audio-alert-watchdog.service
journalctl --user -u measurement-audio-alert-watchdog -n 60
#    b) SILENCE the mbc SIGNAL (not the OBS input's mute), run two passes -> a bucketed SILENT page:
#       mute the Ableton mbc channel on 10.77.7.232, OR OBS-WS SetInputVolume mbc to the minimum
#       (both keep the input PRESENT in the meter stream with zero levels -> SILENT). Then restore ->
#       the next pass logs a machine-channel RECOVERY (no phone ping).
#       DO NOT use SetInputMute for this: muting the OBS INPUT itself typically empties its meter, so
#       the watchdog reads meter_present=0 -> UNKNOWN (no page, fail-safe) and the verify would NOT
#       confirm the SILENT path. An OBS-input mute is deliberately UNKNOWN-not-SILENT: it is an
#       operator toggle, not one of the three chain-failure modes (mic off / Ableton mute / Dante
#       drop), all of which are UPSTREAM of the OBS input and correctly read SILENT.
#    c) or stub the fetch seam against a crafted reading (no live box touched):
MEASUREMENT_AUDIO_FETCH_CMD=/path/to/stub \
  scripts/measurement-audio-alert-watchdog.sh --dry-run     # confirm two passes -> a bucketed alert

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now measurement-audio-alert-watchdog.timer
systemctl --user list-timers | grep measurement-audio-alert-watchdog

# Disable later:
systemctl --user disable --now measurement-audio-alert-watchdog.timer
```

Note: on dev1 the systemd `--user` hardening directives are live-verified INERT no-ops under the box's
unprivileged-userns kernel policy (`.claude/rules/dev1-systemd-user-unit-hardening.md`), so none are
declared here — the sibling units carry none either.

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `MEASUREMENT_AUDIO_REPING_INTERVAL_S` | `600` | re-ping bucket size (s); floored at 60 in the shared helper |
| `MEASUREMENT_AUDIO_CONFIRM_THRESHOLD` | `2` | consecutive SILENT readings before the first page |
| `MEASUREMENT_AUDIO_WS_HOST` | `obs_fleet_host stream` (10.77.9.204) | stream OBS host |
| `MEASUREMENT_AUDIO_WS_PORT` | `4455` | stream OBS WebSocket port |
| `MEASUREMENT_AUDIO_INPUT` | `mbc` | the measurement-audio input name to meter |
| `MEASUREMENT_AUDIO_SAMPLE_S` | `2.0` | InputVolumeMeters sampling window (s) |
| `MEASUREMENT_AUDIO_WS_PASSWORD` | *(empty)* | OBS-WS password (LAN boxes use none) |
| `MEASUREMENT_AUDIO_FETCH_CMD` | *(unset)* | Tier-0 seam: `<cmd> <host> <port>` stdout+exit replace the probe |
| `MEASUREMENT_AUDIO_NOW` | *(unset)* | Tier-0 seam: fixed epoch for deterministic bucket tests |
| `RIG_MODE_PAINTER_IP` / `CAM_PW` | `10.77.9.62` / `newlevel` | cam2 rig-mode probe target + credential |
| `MEASUREMENT_AUDIO_ALERT_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-measurement-audio-alert.state` | per-key confirm/latch state |

## What this does NOT do

- It makes **no box-side change** and takes **no auto-action** — the cure (unmute the mic / Ableton
  channel, fix the Dante route) is a rig-ops call; this watchdog detects and alerts.
- It does not replace the in-RUN #748 preflight — that is the hard gate INSIDE an E2E cycle; this is
  the between-run dev1 → Discord half.
