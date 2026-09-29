---
paths:
  - "scripts/cadence-alert-watchdog.sh"
  - "scripts/lib/cadence-health.sh"
  - "systemd/cadence-alert-watchdog.*"
  - "tests/harness_cadence_health_794.rs"
---

# non-60 source-cadence alert watchdog (#794)

A fourth sibling of the dev1-side alert-watchdog family (network-reach #1001 → frozen-input #1052 →
bundle-state #732 → this). It pages when a CAMERA source's delivered cadence sits sustained away from
60 fps — the gap behind a live-event finding (2026-07-17: ≥2 cameras set to 50 fps into the 60 fps
chain, undetected for weeks; no frame loss, but 5:6 pulldown judder and a misleading blend input).

## The signal: genlock-fifo `received=` DELTA ÷ the audit lines' OWN timestamps — NOT the `@fps` decoration, NOT a wall-clock divisor

- **The `@ X.XXXfps` decoration in the audit line is USELESS per-source.** It is `genlock_video_fps()`
  = the GLOBAL CANVAS fps (obs-source.c: *"one value shared by EVERY genlock source"*), so it reads
  60 (stream) / 30 (strih) for every source regardless of that source's actual delivery. The ONLY
  per-source delivered-rate signal is the `received=` cumulative counter.
- **Rate = Δreceived ÷ Δ(the two matched audit lines' OWN log timestamps). NEVER a wall-clock /
  pass-interval divisor** — this is the #797 "phantom 50.1 fps" avoidance, and it is the load-bearing
  decision. The audit line appends every ~5.017 s; dividing a `received` delta by a 6 s WALL sleep
  spans ~one tick (+301 frames at a true 60) and prints 301/6 = **50.17** — a phantom "50" at EVERY
  true-60 source (the retracted "OBS caps at 50" thesis that cost two days, `.claude/skills/genlock`
  §phantom-50.1). Dividing the same +301 by the lines' own 5.017 s reads ~60. The dev1-side watchdog
  persists `(received, line_ts)` per source per pass and computes the rate across two passes from the
  persisted line timestamps — so even a wall-clock-noisy pass cadence yields an exact, well-averaged
  rate. `scripts/lib/cadence-health.sh` `cadence_measure_fps` is the pure kernel; the phantom-50
  guard test (`tests/harness_cadence_health_794.rs::measure_avoids_the_phantom_50_from_a_single_audit_tick_797`)
  pins it (a single-tick window must read ~60, not ~50).

## Watch STRIH (not stream), expected 60 ± 3, and orthogonal to the frozen/freeze watchdogs

- **strih receives the RAW camera NDI at its native delivery rate** (its genlock FIFO emits
  `genlock-fifo audit 'NDI camN'`); stream only sees 2ME PGM (30 fps by design) + interkom. So
  `CADENCE_BOX` = strih and `CADENCE_SOURCES` = the strih camera labels (set to the live active set —
  the default is a placeholder). 2ME PGM's legitimate 30 fps is why stream is NOT the box to watch.
- **Tolerance ±3 fps** (WRONG outside [57,63]): 59.94 NTSC is a legit "60" and in-band; 50/43 are
  cleanly separated. Over a 5-min pass window the measurement noise is <0.1 fps, so ±3 is headroom.
- **A frozen source (`received` delta 0) is UNKNOWN here, never "wrong 0 fps"** — a freeze is #1052's
  job. `advanced != 1` maps to UNKNOWN in `cadence_classify`, keeping cadence and freeze orthogonal.
- Same shared framework as the siblings: `obs_watchdog_confirm` (2-pass) + `obs_watchdog_alert_throttle`
  (~30 min re-alert, `CADENCE_ALERT_THROTTLE_PASSES=6` at the 5-min cadence), per-source state, the
  #1001 no-double-page read (strih down → SKIP), a fail-loud `require_tools` preflight (a missing
  awk/sshpass/ssh/timeout aborts LOUD rather than reading every probe as a blind zero), and a
  "tap broken" WARN after ~2 h of a listed source emitting no audit line. Alert body is plain-Slovak-
  friendly ("kamera je pravdepodobne prepnutá na 50 fps — prepni ju späť na 60").

## KNOWN BLIND SPOT (a separate follow-up) — the duplication-masked 50→60 "hard layer"

A grabber that upconverts 50→60 by DUPLICATION delivers a padded genuine 60 NDI frames/s, so
`received=` reads a clean 60 and this receiver-side rate tap cannot see it. Detecting that needs
per-frame content hashing (pixel access — cam-box-side is a rig write; receiver-side is heavy), a
categorically different mechanism. Filed as its own issue; this watchdog covers the genuinely-
non-60-DELIVERED case only.

## Two gotchas for the NEXT multi-source sibling watchdog (review-round, #794)

- **Copying a SINGLE-source sibling (frozen-input) to a MULTI-source one: fetch the box log ONCE per
  pass, not once per source.** The frozen-input probe does one `ssh + powershell "gc … -Tail 800"`
  per watched source — invisible at its 1-source default, but a 7-camera cadence scope makes it 7
  identical fetches of the SAME log per pass. Two consequences: pure waste (one tail carries every
  source's audit line → one fetch + N local greps is identical), AND a real budget bug —
  `N × SSH_TIMEOUT` can exceed the unit's `TimeoutStartSec`, so a reachable-but-slow box gets the
  oneshot SIGTERM'd mid-pass. Pattern: a box-level `fetch_box_log <ip>` in `main()` + a pure
  `extract_sample <raw_log> <source>` per source (`CADENCE_PROBE_CMD` is box-level: `<box_ip>`).
- **A blind-tap / silent-UNKNOWN guard must key on ALL fields the measurement needs, not one.** The
  cadence sample is a PAIR (`received=` + line timestamp); keying the tap-broken guard on `received=`
  alone left a hole: a valid `received=` with an unparseable/empty timestamp reset the blind-tap
  counter yet stayed permanently unmeasurable → the source could sit UNKNOWN forever with no page and
  no WARN (the exact invariant the guard exists to uphold). Fix: persist + blind-tap key on a USABLE
  PAIR (recv integer AND ts present). Any "never a silent UNKNOWN" guard: increment the blind counter
  when the sample is missing ANY field it needs downstream, not just the first one you check.

## Tier-0 + ships DISABLED

Pure lib is Tier-0 (`std`/bash, no probe/OBS/rig). `cargo` does NOT run locally here (build-ok
DISABLED #477) — prove behaviour by running `scripts/cadence-alert-watchdog.sh --dry-run` with a
`CADENCE_PROBE_CMD` stub (seed → WRONG-confirm → alert → throttle → recovery), and by sourcing the
lib directly; CI runs the Rust assertions. Units are committed but NOT enabled — install/enable per
`systemd/cadence-alert-watchdog.README.md` (the supervisor sets the live `CADENCE_SOURCES` first).

## Byte-safety of the received= extraction (#1258 layer 2)

`extract_sample` (in `scripts/cadence-alert-watchdog.sh`, NOT `scripts/lib/cadence-health.sh`) reads
the `genlock-fifo audit '<source>': received=` line from the raw `ps_encoded_command`-fetched tail
via `LC_ALL=C grep -aF ... | LC_ALL=C sed -n ...` on every stage (the line-find grep, the received=
digit sed, and the leading-timestamp sed) — NOT plain grep/sed. PowerShell 5.1's `gc` (no
`-Encoding`) re-encodes a non-ASCII glyph anywhere in the fetched tail as invalid UTF-8; in this
fleet's UTF-8 locale, plain grep then flags stdin BINARY (empty extraction) and plain sed's trailing
`.*` leaves line-tail garbage after the digits — either way the sample is worthless. Full root cause
+ the fix pattern: `.claude/rules/mv-reverify-escalate.md` "Layer 2" section (this watchdog is one of
its 3 proven-RED consumers, verified via a Tier-0 fixture with an injected invalid byte). Never
re-add a plain (non-`LC_ALL=C`) grep/sed on this extraction.
