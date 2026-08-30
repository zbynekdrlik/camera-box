---
paths:
  - "scripts/audio-lag-alert-watchdog.sh"
  - "scripts/audio_lag_decision.py"
  - "systemd/audio-lag-alert-watchdog.*"
  - "tests/python/test_audio_lag_*.py"
---

# dev1-side OBS audio-timeline-lag alert watchdog (#1226)

Closes the detection gap behind the **2026-08-30 nedeľná služba** incident: stream OBS's audio
subsystem fell ~24–27 s/min behind realtime from StartStream; `ts_lag_ms` grew to 1 672 741 ms
(27,9 min) and the YouTube stream's A/V desynced for a whole service, because the OBS log line that
screamed it the whole hour was read by nobody. A fifth dev1 alert-watchdog sibling
(network-reach #1001 → bundle-state #732 → cadence #794 → this).

## The signal + the exact log line — parse it PRECISELY

`vendor/obs-studio/libobs/obs-audio.c:698` emits, every 60 s per audio source:
`audio-telemetry #800 '<src>': ts_lag_ms=<int64> buffered_ms=<int> pending=<int> timing_adjust_ms=<int64>`
plus a summary line WITHOUT a quoted name: `audio-telemetry #800: total_buffering=... buffering_source=...`.

- `bundle_state_gather.audio_ts_lag_ms_from_log(text)` → `(max_lag_str, src)`: LAST reading PER
  source, then the **MAX** across sources (all sources lagging equally = a global audio-tick/mix
  pipeline behind realtime, the incident signature). The regex `'([^']*)': ts_lag_ms=(-?\d+)` anchors
  on the closing-quote-then-`: ts_lag_ms=`, so the summary line (no quoted name) never matches.
- **`ts_lag_ms=-1` means audio_ts==0 (source present, no audio timeline yet) — NOT a lag.** It is
  excluded from the max; a source whose newest reading is -1 does not contribute; only-negatives →
  `("","")` (facet omitted, never a fake 0).
- **Scan the TAIL window only.** The #1222 bounded read returns `head + LOG_BOUNDED_READ_SEPARATOR +
  tail`; the facet must reflect CURRENT state, so it `rsplit`s on the separator and scans only the
  tail — a stale HIGH value that survives only in the head (an old recovered episode from the startup
  region) is never reported. A small whole-file log (no separator) is scanned entirely. Reuse the
  SAME bounded `log_text` the other `_from_log` parsers use — never add a second log read.
- Wired into `bundle-state-server.gather_bundle_state` via `_parse_log_facets` (one `obs_log_parse`
  timing) + two `build_bundle_state` kwargs (`audio_ts_lag_ms`/`audio_ts_lag_src`, omit-when-empty).

## The watchdog is DETECTION-ONLY — no auto-action, alert-only

The observed cure was a **PC reboot of a live prod box** (a genuinely destructive owner-call,
`no-destructive-remote-actions.md`); the preventive pre-service reboot is an owner decision on the
ticket. So unlike bundle-state #732 (which auto-restarts the `:8899` task), this watchdog takes NO
action — it pages (throttled) and logs recovery machine-only. Pure decision core
`scripts/audio_lag_decision.py` (`extract_audio_lag` + `classify` + `analyze`, #1199 python-mirror
pattern, pytest Tier-0); orchestrator `scripts/audio-lag-alert-watchdog.sh` curls
`:8899/bundle-state.json` and reuses `scripts/lib/obs-watchdog-decision.sh`
(`obs_watchdog_confirm` 2-pass + `obs_watchdog_alert_throttle` ~1h) VERBATIM.

## Classification + the NEVER-false-page invariant

`SKIP` (fetch failed — box/`:8899` down, defer to #732/#1001) · `UNKNOWN` (fetched but facet absent
— no `#800` line in the tail yet) · `HEALTHY` (lag ≤ `AUDIO_LAG_THRESHOLD_MS`, default 5000) ·
`LAGGING` (> threshold → page after 2-pass confirm). **The only page condition is a
successfully-fetched POSITIVE lag reading**, so a dev1-side path outage makes every fetch fail →
SKIP → no page — which is why this watchdog needs **no** reference-anchor/outage guard (bundle-state
#732 needs one because it restarts + pages on DOWN). SKIP/UNKNOWN HOLD the confirm counter (an
unmeasured pass neither advances nor resets it); HEALTHY resets it.

## #1206 notify discipline + ships DISABLED

ALERT carries a STABLE `--dedup-key "audio-lag-$box"` (no per-pass component — a growing lag EDITS
the one card instead of re-pinging); recovery is `log`-only / machine-channel, never a ✅ phone ping.
`tests/python/test_notify_dedup_key_sweep_1206.py` auto-discovers this script and enforces both.
Units are committed but NOT enabled — install/verify/enable per
`systemd/audio-lag-alert-watchdog.README.md` (the supervisor's step).

## Tier-0 verify (no cargo)

`python3 -m pytest tests/python/test_audio_lag_*.py` (parser + decision + CLI contract) +
`test_bundle_state_server_log.py` (the wiring flows the facet through gather); `bash -n` +
`shellcheck -S warning` on the watchdog; a stubbed-`curl` `--dry-run` driver proves seed → 2-pass
confirm → alert → throttle → recovery → SKIP → UNKNOWN. CI runs nothing new for this (pure
python/bash).
