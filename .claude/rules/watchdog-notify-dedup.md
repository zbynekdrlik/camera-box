---
paths:
  - "scripts/*-watchdog.sh"
  - "scripts/*-alert-watchdog.sh"
  - "scripts/cam-disk-guard.sh"
  - "scripts/rig-status.py"
  - "scripts/av_sync_measure.py"
  - "tests/python/test_notify_dedup_key_sweep_1206.py"
---

# Alert-watchdog Discord notify: stable --dedup-key + machine-channel recovery (#1206)

Every `python3 "$NOTIFY" notify --body ...` call in an alert-watchdog is a phone ping to the owner.
Keyless notify is the #1 fleet phone-flood (airuleset #704/#705: this repo was 76% of delivered
pings — one stuck state re-pinged ~288×/day, because airuleset's own auto-dedup can only collapse a
KEYLESS body into a ~5-min window). The doctrine (analyze-not-ping, airuleset #704/#693):

## The two rules — apply to EVERY new watchdog notify call-site

1. **ALERT class → a STABLE per-incident `--dedup-key`.** A genuine actionable incident (🚨 / ⚠️
   degraded/tap-blind / 🛟🧹 one-shot auto-action on prod) pings ONCE; a repeated IDENTICAL state
   then EDITS the existing airuleset card instead of re-pinging (its 14-day marker TTL). The key
   MUST be:
   - **Stable across repeats of the SAME incident** — no per-pass component (no timestamp, no
     fps/count that flips across a rounding boundary), so the watchdog's own ~20-min throttle
     re-fire becomes a silent card edit, not a new ping.
   - **Distinct per genuinely-different incident** — include the box/source/verdict/leg where the
     watchdog is per-box/per-source (`obs-liveness-$box`, `asio-starve-$source`,
     `frozen-input-$RECEIVER_NAME-$source`, `optical-chain-$verdict`, `avsync-heartbeat-$leg`), and
     use a distinct slug per distinct alert TYPE in a multi-alert script
     (`imag-power-{journal,throttle,render-churn}`, `avsync-lineup-{liveness,preflight-nogo}`).
   - Built from a variable actually IN SCOPE at that call-site. For a shared emit helper
     (`fire_notify`, `alert`, `throttled_notify`, `process_leg`), pass/derive the key there — never
     re-inline the notify.

2. **RECOVERY / STATUS class → NEVER a phone ping.** The ✅ "back to normal / serving again / OK
   again / reachable again" latch pings, "still down" repeats, and periodic OK/health lines go to
   the MACHINE channel only: keep (or add) a `log "RECOVERY: ... machine-channel only (#1206)"`
   journal line and DROP the `notify --body` call. The dry-run `[dry-run] WOULD send recovery ...`
   decision log stays (tests pin it). Do NOT change the recovery DECISION logic (`*_recovery_decision`).

The emoji is the discriminator in practice: **🚨/⚠️/🛟/🧹 = ALERT (keyed ping); ✅ = RECOVERY
(machine-channel, no ping)**.

## Production-critical class: time-bucketed re-ping (owner ruling 2026-09-13, #1307)

Rule 1 above ("a STABLE per-incident `--dedup-key`, no per-pass component") is the DEFAULT and holds
for every watchdog EXCEPT one deliberately-carved class. Owner ruling (ROZHODNUTÉ on #1307, verbatim:
*„aj ntp aj ostatne veci bez ktorych nevie produkcia bezat spravne musi notifikovat … nech kazdu
minutu chodia notifikacie ze nemaju dante clock … byt o tom dokolecka notifikovany"*): a
**production-critical** condition — one the fleet cannot run production without, whose loss is
INVISIBLE without a page (a lost dante clock, a foreign/missing grandmaster, an NTP step-storm) — must
be **RE-pinged repeatedly while it PERSISTS**, not paged once and then silently card-edited forever.

Mechanism (no new notify channel, no raw webhook): keep `airuleset.py notify --dedup-key`, but make
the key **time-bucketed** — `<incident-key>-<floor(now/REPING_INTERVAL_S)>` (`REPING_INTERVAL_S`
default 600 s, floored at 60 s). Within one bucket an identical state still EDITS the card (no flood);
every new bucket is a FRESH ping. Recovery stays exactly rule 2 — ONE machine-channel log line, never
a phone ping.

**The bucket key is built by ONE shared helper — the ONLY sanctioned way to time-bucket (#1308).**
Never hand-roll the `-<floor(now/interval)>` suffix or a second bucketing implementation:
- **bash:** `scripts/lib/obs-watchdog-decision.sh :: watchdog_notify_key <incident-key> <now_epoch>
  [interval_s]` — every alert-watchdog already sources that lib. Wrap the existing stable key:
  `--dedup-key "$(watchdog_notify_key "network-reach-$box" "$(date +%s)")"` (the interval comes from
  `$REPING_INTERVAL_S`, the ONE shared env name, default 600, floored 60). (`dantesync-clock-alert-
  watchdog.sh` buckets via its `bucketed_key` helper, which also calls `watchdog_notify_key`.)
- **python:** `scripts/watchdog_reping.py :: notify_key(base, now, interval)` — the byte-for-byte twin
  (a parity pytest diffs the two); `dantesync_clock_decision.dedup_key` delegates to it.

**The production-critical class (#1308) is these 12 dev1 watchdogs** — the faults the fleet cannot run
production without, invisible without a page:

| watchdog | ticket | fault |
|---|---|---|
| `dantesync-clock-alert-watchdog.sh` | #1307 | PTP clock loss / GM move / DNS / NTP storm / dantesync-dead |
| `genlock-lock-alert-watchdog.sh` | #1299 | fleet genlock UNLOCKED/DEGRADED |
| `network-reach-alert-watchdog.sh` | #1001 | strih/stream unreachable |
| `bundle-state-alert-watchdog.sh` | #732 | :8899 bundle-state server down |
| `obs-liveness-watchdog.sh` | #391 | broadcast-OBS render wedge |
| `audio-lag-alert-watchdog.sh` | #1226 | OBS audio-timeline lag / band drift |
| `asio-starve-alert-watchdog.sh` | #1023 | ASIO source starved |
| `vb-matrix-alert-watchdog.sh` | #1227 | VB-Matrix down |
| `ndi-portmap-alert-watchdog.sh` | #1181 | NDI sender port-map moved |
| `avsync-heartbeat-alert-watchdog.sh` | #812 | A/V-sync heartbeat stale |
| `imag-obs-alert-watchdog.sh` | #882 | imag OBS down / latency-drift / restart-storm |
| `measurement-audio-alert-watchdog.sh` | #1310 | mbc measurement-audio chain digital-silent (TEST-gated) |

(`measurement-audio-alert-watchdog.sh` is EVENT-gated on `rig-mode-state.sh` like splitter-port #1290
— the QPSK marker only sounds in TEST — but its FAULT is production-critical: a silent measurement
instrument means the next production's A/V-sync can't be verified. The rig-mode gate and the
fault-criticality axis are ORTHOGONAL — this one is TEST-gated AND time-bucketed.)

The DIAGNOSTIC / TEST-mode watchdogs stay one-ping-per-incident (a stable key, NO bucket): cadence
(#794), frozen-input (#1052), splitter-port (#739), grabber-stuck (#1128), imag-power (#1040),
ndi-halving (#1203), optical-chain (#860), obs-burn-reconcile (#1060), mv-fps (#771), av-step (#1267),
netcfg-audit (#797), rig-status (#787). Do NOT time-bucket any of these — the exception is NARROW.

The sweep below allowlists the 11 EXPLICITLY (CLASS-based) and **rejects a bucketed key in any
NON-allowlisted script**, so the exception is intentional and visible, never a silently-weakened
invariant.

**Delivery-layer caveat (#1308):** wrapping the key does NOT change a watchdog's own confirm/throttle
detection — the 10 wrapped bash watchdogs still gate their notify CALL through `obs_watchdog_alert_
throttle`, so their effective re-ping cadence is `max(throttle interval, bucket interval)` (the
bucketed key just turns each throttled re-fire into a fresh ping instead of a silent card edit). Only
`dantesync-clock-alert-watchdog.sh` fires every confirmed pass (no throttle), so the bucket alone sets
its cadence. Tightening a specific watchdog to the exact 600 s cadence is a per-watchdog throttle tune,
out of #1308's delivery-layer scope.

## Enforcement

`tests/python/test_notify_dedup_key_sweep_1206.py` is a Tier-0 static sweep that auto-discovers
every `scripts/**` file emitting `notify --body` (and rig-status.py's `subprocess.run` list form)
and asserts (A) every surviving emit carries `--dedup-key`, (B) no emit body contains ✅. A new
watchdog that adds a keyless notify, or phone-pings a ✅ recovery, fails this test in CI. It joins
bash `\` line-continuations, so the `--dedup-key` may sit on its own continuation line.

## Scope

Delivery layer ONLY. Detection/confirm/throttle lives in `scripts/lib/obs-watchdog-decision.sh`
(shared) + per-script `*_recovery_decision` — do not touch it for a notify change. NEVER edit
airuleset itself; `--dedup-key` is an existing airuleset `notify` flag ("same key sends once").

## A RAW-webhook emitter is INVISIBLE to this sweep — route the default through airuleset notify (#1207)

The sweep above discovers ONLY `notify --body` (bash) / `subprocess.run([... "notify" ... "--body"
...])` (py) call-sites. An alert emitted through a RAW Discord webhook — a bare
`urllib.request.urlopen()` / `requests.post()` to a `--webhook` URL, with no airuleset in the path —
is completely invisible to it, so it gets NO `--dedup-key` enforcement and no analyze-not-ping
doctrine, and it re-POSTs a repeated identical state with nothing collapsing it. `av_sync_measure.py`
was the last such emitter (#1207); the 22 systemd watchdogs never had this shape.

The fix pattern (delivery layer only — never touch the detection/threshold logic):

1. **Route the DEFAULT through airuleset notify with a stable per-kind `--dedup-key`** (the two rules
   above apply unchanged: ALERT → stable key like `av-sync-measure-verdict`; a ✅ recovery would go to
   the machine channel). Add ONE `deliver_alert(args, kind, text)` seam and route every call-site
   through it, so the key is derived in one place.
2. **Write the airuleset call as a LITERAL `subprocess.run([...])` with `(` immediately followed by
   `[`** — that exact shape is what makes THIS sweep's `.py` discovery regex
   (`subprocess\.run\(\[(.*?)\]`) auto-find and enforce it. A variable-list form
   (`cmd = [...]; subprocess.run(cmd)`) is NOT discovered — the sweep would silently skip it, exactly
   the invisibility this section is about. (`test_sweep_covers_the_known_alert_watchdogs`'s `len >= 20`
   + expected-subset stay green; adding one more emitter only grows the set. av_sync_measure is now the
   24th.)
3. **If the tool keeps a raw `--webhook` as an EXPLICIT opt-in override** (a manual/hand-run tool),
   that raw branch still can't carry `--dedup-key`, so give it its OWN simple in-process per-kind
   throttle (`_WEBHOOK_LAST_SENT` + `WEBHOOK_THROTTLE_S`, ~20 min mirroring the watchdogs' re-fire
   cadence) so a sustained state in a `--loop` doesn't re-POST every round.

Test the seam behaviorally (monkeypatch `subprocess.run` + `notify_discord`), not just via the static
sweep — the sweep proves the key is PRESENT, the behavioral test proves the DEFAULT actually routes
to airuleset and the webhook branch actually throttles (see
`tests/python/test_av_sync_measure_notify_dedup_1207.py`). Beware a NEW default-path delivery that
fires on a path an existing test already exercised with `webhook=None` (av_sync_measure's
`one_measurement` now delivers on the default path when `|offset| >= threshold`, so the #805
calibration-log tests had to stub `deliver_alert` to avoid firing a real notify).
