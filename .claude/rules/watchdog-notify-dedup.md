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
