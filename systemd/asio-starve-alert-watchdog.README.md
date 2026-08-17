# asio-starve-alert-watchdog (#1023) — install / live-verify / enable (SHIPS DISABLED)

DEV1-side ASIO-source-starved alert. A sibling of the dev1 alert-watchdog family (network-reach
#1001 / frozen-input #1052 / bundle-state #732 / cadence #794). The units are COMMITTED but NOT
enabled — the supervisor installs + enables after a live-verify. This watchdog makes **no** rig-side
change: it only reads stream's OBS log over ssh from dev1 and calls `airuleset.py notify`.

## What it detects

When stream OBS starts BEFORE its ASIO device/matrix is ready, an ASIO source connects but its audio
callback perpetually STARVES (no samples) → the source is silent and only an OBS reset fixes it. The
vendored genlock build prints, once per 60 s per source, to the stream OBS log:

```
asrc: source '<name>' … cumulative_correction=…ms/60s starved_blocks=N (#803/#806/#960)
```

`starved_blocks=N` is PER-INTERVAL (reset-on-read). A healthy source reads `starved_blocks=0` every
interval; a source that started before its ASIO matrix was ready reads ~2946 (≈100 % of ~2900 audio
callbacks/60 s) SUSTAINED, while a sibling ASIO source on a different device stays at 0. This
watchdog pages a source only when its `starved_blocks` sits **≥ threshold** (default 1000, ≈34 % of
callbacks) AND at least one OTHER watched source is proven HEALTHY (~0) — that healthy sibling proves
the box's audio subsystem is fine and the starvation is source-specific, not a box-wide outage
(obs-liveness #391 / audio-presence own a box-wide outage — never double-paged here).

Reproduced LIVE 2026-08-17: `'ASIO Input Capture'` read `starved_blocks≈2946` every interval for
11.5 h on the stream box while `'mbc'` read 0.

## The watch set — real ASIO inputs only (≥2 for the discriminator)

Default `ASIO_STARVE_SOURCES='ASIO Input Capture;mbc'` — the two real ASIO inputs on the stream box.
The synthetic asrc repro sources (`test-audio` / `fallback repro`, which also starve by design) are
EXCLUDED simply by not being listed. **≥2 sources must be listed** for the healthy-sibling
discriminator to work (a lone source can never prove its own starvation is source-specific — it
would classify UNKNOWN and never page). Read the live asrc source names on the stream box:

```bash
sshpass -p newlevel ssh -o StrictHostKeyChecking=no newlevel@10.77.9.204 \
  'powershell -NoProfile -Command "gc (gci $env:APPDATA\obs-studio\logs\*.txt | sort LastWriteTime | select -last 1).FullName -Tail 400"' \
  | grep -oE "asrc: source '[^']+'" | sort -u
```

## Live-verify from dev1 BEFORE enabling (a dry-run against the real stream log)

```bash
cd ~/devel/camera-box
scripts/asio-starve-alert-watchdog.sh --dry-run
# Expect: 'mbc' -> OK. If 'ASIO Input Capture' is currently starved on the box, pass 1 seeds
# confirm=1 (holds), a second run ≥5 min later reaches confirm=2 -> "[dry-run] WOULD alert". A healthy
# box reads OK for every source. A `*_PROBE_CMD` stub can feed a fixture log for an offline smoke test.
```

## Enable on dev1 (after the live-verify)

```bash
cp systemd/asio-starve-alert-watchdog.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now asio-starve-alert-watchdog.timer
```

Runs entirely dev1-side; nothing is deployed to stream. Tunables (env, override in a drop-in):
`ASIO_STARVE_BOX`, `ASIO_STARVE_SOURCES`, `ASIO_STARVE_THRESHOLD` (default 1000),
`ASIO_STARVE_ALERT_CONFIRM_THRESHOLD` (2), `ASIO_STARVE_ALERT_THROTTLE_PASSES` (6 ≈ 30 min),
`ASIO_STARVE_TAP_BROKEN_THRESHOLD` (24 ≈ 2 h).

## Scope / out of scope

- The starvation originates in the closed VB-Matrix / Dante ASIO plugin UPSTREAM of the vendored
  genlock build (no obs-asio plugin in `vendor/`), so it is not fixable in our code — this watchdog
  DETECTS the signature and pages with the OBS-reset cure (alert-only).
- A launch-time guard already exists (`scripts/launch-obs-genlock.sh` #786 audio-buffering relaunch
  loop); this watchdog covers the whole run, not just the launch instant (the live incident proved a
  box can pass launch and starve for 11.5 h afterward).
- A box-wide audio outage (every watched source starving) is UNKNOWN here (owned by obs-liveness
  #391 / audio-presence) — never double-paged.
