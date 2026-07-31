---
paths:
  - "src/self_heal_attribution.rs"
  - "src/frozen_leg.rs"
  - "src/capture_rate_health.rs"
  - "src/capture_rate_selfheal.rs"
  - "scripts/lib/self-heal-attribution.sh"
  - "scripts/lib/capture-rate-guard.sh"
---

# A self-heal reset must never be misreported as a frozen camera (#895)

## Two WARN texts, one underlying event — a check that greps only ONE band misses the other

`capture_rate_selfheal` triggers on EITHER of two independent bands in `src/main.rs`:
`jitter_confirmed` (the wide, per-grabber-model `#656` tolerance) OR `sustained_confirmed` (a
narrower, longer-window `#717` tolerance for a CHRONIC deviation that stays inside the wide jitter
envelope). Each band logs a DIFFERENT literal WARN text
(`"#656 capture-delivery-rate DEFECTIVE"` vs `"#717 capture-delivery-rate SUSTAINED defect"`), but
both funnel into the SAME `SelfHealDecision::Heal` reset — which logs a THIRD, shared line:
`"#663 self-heal: USB reset attempt #N succeeded"`.

`scripts/lib/capture-rate-guard.sh`'s existing mid-recording recheck (`[7b/8]` in
`recording-e2e.sh`, a separate, already-shipped ticket) greps ONLY the `#656` jitter text. A reset
triggered via the `#717` sustained band alone — exactly cam1's own characterized deviation (~2-3%,
inside the wide jitter tolerance but past the narrower sustained one) — is therefore INVISIBLE to
that check, reaches `recording-verdict.rs` unflagged, and its resulting duplicate/stale frames get
classified `frozen_leg` on the camera.

**The generalizable lesson: when a mid-recording forensic check greps for a detection-band's WARN
text to catch a downstream EVENT, and the event has more than one independent trigger path, key on
the EVENT ITSELF (the one line every trigger path shares), never on any single upstream band's
wording.** `self_heal_reset_grep_pattern` (`scripts/lib/self-heal-attribution.sh`) matches
`"#663 self-heal: USB reset attempt #[0-9]+ succeeded"` — the shared `SelfHealDecision::Heal`
line — so a hypothetical future THIRD detection band is automatically covered with no further
harness change. If you ever add a fourth trigger band to `capture_rate_selfheal`, verify it still
funnels through that exact log line before assuming this coverage holds.

## ALLOW, never SUPPRESS — the correlation module doesn't silence the underlying defect

`src/self_heal_attribution.rs::attribute_self_heal` re-attributes a classified-`Frozen` window to
`self_heal_reset` ONLY when a same-camera reset event correlates (within
`SELF_HEAL_CORRELATION_MARGIN_NS`, 5s either side of the window). It does NOT drop the event —
`any_self_heal()` stays true (gating `all_pass`) even for an event that never correlates to any
classified window, and even when the correlated window's stale frames were reattributed away from
`frozen_leg`. The run still fails; only the LABEL changes from "camera fault" to "run-integrity
event" (see `recording-verdict.rs`'s `report["self_heal_reset"]` JSON block). Never change this
module to unconditionally swallow a self-heal event just because it also correlates to a window —
the underlying rate defect (#728) is real and unresolved, and silently tolerating it would be
exactly the "suppress" branch the ticket's own design note rejected.

## Scope the mid-recording scan to EVERY active camera, not just the source camera

`capture_rate_selfheal` runs identically on every `camera-box` process — a scan that only queries
one box (mirroring the pre-existing `capture-rate-guard.sh` check's `CAM1_IP`-only scope) misses
the same class of misattribution on any OTHER active camera. `_self_heal_reset_scan` in
`recording-e2e.sh` sweeps `CAM1_IP` AND every `CAMBOX_SECONDARY_DEPLOY` entry when
`ALL_CAMBOX=1` — mirror this loop shape (not the older single-box check) for any FUTURE
mid-recording forensic scan added to this harness.

## Bash pipeline gotcha: `grep -E pattern | awk ...` must survive ZERO matches under `pipefail`

`self_heal_reset_events_from_output` (the common, expected case is NO self-heal fired) pipes
`grep -E "$pattern"` into `awk`. Under `set -o pipefail` (which `recording-e2e.sh` — and this
lib's own test harness — both set), a `grep` that finds nothing exits 1, and `pipefail` propagates
that 1 as the WHOLE pipeline's exit status even though the `awk` stage itself succeeded. Guard the
grep stage explicitly: `{ grep -E "$pattern" || true; }` — never rely on the CALLER always wrapping
the whole `$(...)` substitution in `|| true` (this repo does that at some call sites, e.g.
`capture-rate-guard.sh`'s existing check, but a pure lib function meant to be called directly, not
only embedded in a larger remote command string, should be safe on its own).
