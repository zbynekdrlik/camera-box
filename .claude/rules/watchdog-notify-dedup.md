---
paths:
  - "scripts/*-watchdog.sh"
  - "scripts/*-alert-watchdog.sh"
  - "scripts/cam-disk-guard.sh"
  - "scripts/rig-status.py"
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
