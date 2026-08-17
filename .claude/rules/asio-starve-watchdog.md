---
paths:
  - "scripts/asio-starve-alert-watchdog.sh"
  - "scripts/lib/asio-starve-health.sh"
  - "systemd/asio-starve-alert-watchdog.*"
  - "tests/harness_asio_starve_health_1023.rs"
---

# ASIO-source-starved alert watchdog (#1023)

A fifth sibling of the dev1-side alert-watchdog family (network-reach #1001 → frozen-input #1052 →
bundle-state #732 → cadence #794 → this). It pages when a stream-OBS ASIO source is SILENT because
OBS started before its ASIO device/matrix was ready — the source's audio callback perpetually
starves (no samples) and only an OBS reset fixes it (the ticket's own confirmed cure).

## The tap: the `asrc: source '<name>' … starved_blocks=N` line — a NEW parse, not jitter_audit.rs

The vendored genlock build (`vendor/obs-studio/libobs/obs-source.c:4243`) prints, once per
`ASRC_LOG_INTERVAL_S` (=60 s) per source, to the stream OBS log:

```
HH:MM:SS.mmm: asrc: source '<name>' estimated=…ppm applied=…ppm outer_bias=…ppm cumulative_correction=…ms/60s starved_blocks=N (#803/#806/#960)
```

- **`starved_blocks=N` is PER-INTERVAL (reset-on-read**, `vendor/obs-studio/libobs/media-io/asrc-compensator.c:232`
  zeroes it whenever `asrc_compensator_should_log()` returns true) — NOT a lifetime cumulative. So the
  NEWEST line's value is a self-contained 60 s measurement; the watchdog reads it DIRECTLY, with NO
  prev/curr delta (unlike the #794/#1052 `received=` counter, which is cumulative). Do not "fix" it
  into a delta.
- **This `asrc:` line is a DIFFERENT family from `genlock-fifo audit '<src>':` (input) and
  `genlock-ndi-output/-filter audit` (send-side).** `src/jitter_audit.rs` parses the latter two, NOT
  `asrc:` — so the tap is a fresh `grep -F "asrc: source '<name>'"` in `scripts/lib/asio-starve-health.sh`
  (`asio_starve_parse_blocks`), anchored on the trailing `'` so `'mbc'` never matches a `'mbc2'` line.
- **The signature:** healthy source = `starved_blocks=0` every interval; a source that started before
  its ASIO matrix was ready = ~2946 (≈100 % of ~2900 callbacks/60 s) SUSTAINED. Reproduced LIVE
  2026-08-17 on the stream box: `'ASIO Input Capture'` ≈2946 for 11.5 h while `'mbc'` = 0. It is a
  HIGH-AND-SUSTAINED per-interval value, not a slow climb; the dispatch's "climbing" wording is
  imprecise — the value hovers 2945–2996 per interval, never a monotonic rise.

## The DISCRIMINATOR: healthy sibling = per-source defect, NOT box-wide (never double-page)

A watched source pages `STARVED` only when its `starved_blocks ≥ threshold` (default 1000, ≈34 % of
callbacks; observed separation is 0 vs ~2946 so the exact value has huge margin) AND at least one
OTHER watched source is proven HEALTHY (~0). The healthy sibling proves the box's clock/OBS/audio
subsystem is fine and the starvation is SOURCE-SPECIFIC — exactly the startup-order defect. If EVERY
watched source starves (no healthy sibling) → `UNKNOWN`, **not** a page: a box-wide audio outage is
owned by obs-liveness #391 / audio-presence, and turning this precise per-source discriminator into a
box-wide alarm would double-page. Symmetric: any listed source can be the starved one; its siblings
are the reference. ⇒ **≥2 sources must be listed** (`ASIO_STARVE_SOURCES`, default
`ASIO Input Capture;mbc`); a lone source can never prove its own starvation is source-specific.

## Scope + framework reuse (nothing reinvented)

- **Watch REAL inputs only.** The synthetic asrc repro sources `test-audio` / `fallback repro` ALSO
  read ~2946 by design — they are EXCLUDED simply by not being in `ASIO_STARVE_SOURCES`. Do not watch
  all `asrc:` sources blindly.
- **Alert-only, OBS-reset cure, ships DISABLED.** The starvation is in the CLOSED VB-Matrix/Dante ASIO
  plugin UPSTREAM of the vendored build (no obs-asio plugin in `vendor/`), so it is not fixable in our
  code — detect + page (`scripts/launch-obs-genlock.sh --box stream --force`). A dev1 timer has no
  session-aware win-* MCP to restart the live stream OBS, so it never auto-restarts (same as
  obs-liveness #391 / obs-session #979).
- **The launch-time #786 guard is COMPLEMENTARY, not a substitute.** `launch-obs-genlock.sh`'s #786
  audio-buffering relaunch loop covers the launch INSTANT; this watchdog covers the whole run (the
  live box passed launch and starved 11.5 h afterward).
- Same shared framework as the siblings: `obs_watchdog_confirm` (2-pass) + `obs_watchdog_alert_throttle`
  (~30 min re-alert) from `scripts/lib/obs-watchdog-decision.sh`, the #1001 no-double-page read
  (`alerted_stream` → box down = SKIP), a fail-loud `require_tools` preflight, one flat non-nested
  ssh+powershell OBS-log tail (`$env:APPDATA` has no spaces → no inner double-quotes), per-source
  state, a "tap broken" WARN after ~2 h of a listed source emitting no `asrc:` line, and a recovery
  ping. Pure lib is Tier-0; `tests/harness_asio_starve_health_1023.rs` sources it + fixtures. cargo
  does not run locally (build-ok DISABLED #477) — prove it via `--dry-run` with a `*_PROBE_CMD` stub,
  or `cargo test --no-run` + running the compiled binary directly.
