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
`any_self_heal()` stays true even for an event that never correlates to any classified window, and
even when the correlated window's stale frames were reattributed away from `frozen_leg`. Only the
LABEL changes from "camera fault" to "run-integrity event" (see `recording-verdict.rs`'s
`report["self_heal_reset"]` JSON block). Never change this module to unconditionally swallow a
self-heal event just because it also correlates to a window — the underlying rate defect (#728) is
real and unresolved, and silently tolerating it would be exactly the "suppress" branch the ticket's
own design note rejected. (`any_self_heal()`/`any_frozen()` no longer fold into `all_pass` as of
#914 below — but they still stay true/computed for exactly this reason: the run-integrity signal
must never be silently dropped, only decoupled from the pass/fail decision.)

## #914 (2026-08-01) — frozen_leg/self_heal_reset became report-only; the pure-decision seam this created

cam1's ShadowCast 2 grabber hardware defect (#909) fails the fused verdict's `overall_pass` on a
hardware fault completely unrelated to whatever the PR's own diff changed (a ~5.5min E2E window has
a high chance of catching one of cam1's 0.6-8s USB-reset freezes). Per the user's standing
gate-relax-and-move-forward directive (2026-07-31, mirrors #889's report-only pattern and #861's
caller-only decoupling): `SelfHealAttributionReport::overall_pass_contribution()` is now hardcoded
`true` — `any_frozen()`/`any_self_heal()` above are COMPLETELY UNCHANGED (still fully computed,
printed, JSON-reported with a new `gates_overall_pass: false` field mirroring `all_cambox_av_sync`'s
#861 shape), only the CALLER (`recording-verdict.rs`) stopped ANDing them into `all_pass`. Restore
path on #905: flip the method body back to `!any_frozen() && !any_self_heal()` once cam1 is
physically replaced and a stable week passes with no self-heal escalations — a one-line change,
which is the whole reason this method exists as its own seam rather than inlining the decision at
each call site.

**Testing a probe-gated report-only decoupling with no local run path — the DIFFERENTIAL fixture
technique.** `recording-verdict.rs` is `required-features = ["probe"]` (this repo's Local Build
Policy — see the project CLAUDE.md), so any end-to-end test added to it has ZERO local
compile/test path; the first time it actually runs is CI. Rather than asserting an ABSOLUTE
`overall_pass == true` on a hand-built fixture (which silently assumes every OTHER unrelated gate
in that huge function also happens to pass with your minimal fixture — fragile, and you cannot
verify it locally before pushing), build TWO otherwise-IDENTICAL fixtures — one WITH the
term-under-test firing, one WITHOUT — and assert `overall_pass` is IDENTICAL between them. This is
exactly `#861`'s own precedent
(`all_cambox_av_sync_gate_failure_no_longer_forces_the_overall_verdict_to_fail_861`) and it fully
sidesteps needing to know or guess the absolute pass/fail value of the OTHER gates — see
`frozen_leg_and_self_heal_reset_no_longer_gate_the_overall_verdict_914` in
`src/bin/recording-verdict.rs` for the worked example (a genuinely HARD-FROZEN window forced via 5
duplicate-tick frames at density 0.20, well above `frozen_leg::FROZEN_DENSITY_THRESHOLD`, plus an
unattributed self-heal event on a cambox name that never appears in the schedule).

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
