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
`recording-e2e.sh`, a separate, already-shipped ticket) originally grepped ONLY the `#656` jitter
text. A reset triggered via the `#717` sustained band alone — exactly cam1's own characterized
deviation (~2-3%, inside the wide jitter tolerance but past the narrower sustained one) — was
therefore INVISIBLE to that check, reached `recording-verdict.rs` unflagged, and its resulting
duplicate/stale frames got classified `frozen_leg` on the camera.

**UPDATE (issue 992 / PR #993, 2026-08-05) — the `[7b/8]` check now reads BOTH journald AND the
burn-instance's own log (`/tmp/cbox-burn.log` — the E2E harness stops `camera-box.service` and
runs the burn as a transient systemd-run unit logging straight to that file, so journald alone is
blind to the actual recording window), and its bands are SPLIT by severity:**
- **HARD (`exit 1`):** `capture_rate_defect_grep_pattern_hard()` = `#656 ... DEFECTIVE` |
  `#971 ... CHRONIC sustained-band DEFECTIVE` | `#663 self-heal: USB reset attempt` — genuine
  defect declarations plus the shared reset EVENT line (the lesson below, applied).
- **REPORT-ONLY (loud `WARNING #992:` line, never aborts):** `capture_rate_sustained_band_grep_pattern()`
  = `#717 ... SUSTAINED band confirmed` — informational by design (the issue-909 section below:
  the decimation gate absorbs over-rate; dupe-preferring as of issue 889), and CHRONIC on the
  ShadowCast (redevelops ~2 min after any device open), so hard-failing on its mere presence made
  the gate permanently red and aborted runs before the verdict — the copies/gaps windows in the
  verdict are the real harm arbiter. The two bands are grepped SEPARATELY, hard first, so a
  `tail -1` landing on a sustained line can never mask a reset.

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
own design note rejected. (`any_self_heal()`/`any_frozen()` fold into `all_pass` AGAIN as of issue
905 item 2 — #914 below temporarily decoupled them while cam1's grabber was unresolved; either way
they stay true/computed for exactly this reason: the run-integrity signal must never be silently
dropped.)

## #914 (2026-08-01) — frozen_leg/self_heal_reset became report-only; the pure-decision seam this created

**RESTORED by issue 905 item 2 (2026-09-02): `overall_pass_contribution()` is `!any_frozen() &&
!any_self_heal()` again and the JSON `gates_overall_pass` reads `true`. The account below describes
the #914 report-only ERA; the restore precondition — a green E2E series with `frozen == []` and a
clean `self_heal_reset` — is now met, and the seam described here is exactly what made the restore
a one-line flip.**

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
`frozen_leg_and_self_heal_reset_restored_to_blocking_905` in
`src/bin/recording-verdict.rs` for the worked example (a genuinely HARD-FROZEN window forced via 5
duplicate-tick frames at density 0.20, well above `frozen_leg::FROZEN_DENSITY_THRESHOLD`, plus an
unattributed self-heal event on a cambox name that never appears in the schedule).

## #909 (2026-08-01) — the SUSTAINED band's own escalation was the harm; decouple it from the reset, don't chase the "hardware"

The #914 report-only decoupling above treated `frozen_leg`/`self_heal_reset` as an unavoidable
hardware fault to tolerate. Follow-up forensics on the SAME cam1 incident (issue 909) found the
actual root cause one layer upstream: `should_trigger_selfheal` OR-combines TWO independent bands
(jitter #656 + sustained #717) into ONE `SelfHealDecision::Heal` USB reset. cam1's chronic
62-64fps deviation trips ONLY the sustained band (the jitter band's tolerance was deliberately
widened by #685 so ShadowCast 2's model-typical wobble never reaches it) — and the genlock
decimation gate in `src/main.rs` (emit the first capture at/after each DanteSync wall-clock
boundary, drop the rest) already absorbs ANY capture over-rate into exact NDI output BY DESIGN.
So the sustained-band trigger was resetting a card for behavior the appliance's own architecture
already neutralizes — the USB reset firing mid-measured-window was the actual defect the E2E gate
was catching, not a hardware fault worth replacing the card over.

**Fix:** `should_trigger_selfheal(jitter_confirmed, _sustained_confirmed)` now returns
`jitter_confirmed` only — the sustained band is still fully computed and logged
(`tracing::info!`, informational-only) at its `src/main.rs` call site, never silently dropped,
just decoupled from the reset action (the same "ALLOW never SUPPRESS" shape as #914 above). Only
a genuinely out-of-envelope jitter-band deviation still triggers an actual USB reset.

**The generalizable lesson, sharpened:** before accepting "the hardware is failing" as the
conclusion from a self-heal/gate-failure investigation, check whether the SELF-HEAL ACTION
ITSELF (not the condition it fires on) is the thing causing the observed harm — and whether an
existing, unrelated piece of architecture (here: the decimation gate, built for an entirely
different reason — genlock alignment) already tolerates the condition the action was reacting to.
#914's report-only bypass was the right STOPGAP while that wasn't yet understood; #909 is the
actual fix, and #914's bypass stays in place (restore path on #905) independent of this — it
guards a DIFFERENT residual risk (a genuine jitter-band device fault still slipping through a
short E2E window), not the sustained-band case this commit resolves.

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

## #1034 (2026-08-14) — re-tightening a per-model RESET floor + recognizing a card swap

Two reusable findings from the capture-rate lane re-tighten:

**1. `SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT` is a RESET floor — keep it above the CLASS
characteristic envelope, never just the current measured spread.** This constant gates
`jitter_confirmed` → a USB reset, and reset-spam is a real harm (issue-909: each spurious reset is
an ~8.3s `frozen_leg` misclassification). So when "re-measure and shrink" a per-model tolerance:
the shrink is bounded BELOW by the model CLASS's historical characteristic envelope (ShadowCast 2:
~55-64fps against 60.000 = up to ~8.33%, pinned in
`live_shadowcast2_characteristic_readings_are_not_deviant_under_its_model_tolerance`), NOT by the
current live spread (2026-08-14: cam1 max 4.67%, cam2 max 2.83%). #1034 tightened 10%→**9%** (above
8.33%) as an INTERIM — a 4-5% floor would sit at/below cam1's own measured max and re-introduce
reset-spam. The REAL shrink to base 1% is delivered per-box by the card SWAP (finding 2), not by
squeezing the shared reset floor. Verify no existing characteristic-reading test breaks at the new
value before shipping.

**2. `grabber_model_from_card_name` needs WHITESPACE-COLLAPSED matching.** cam3's live V4L2 `card`
string after its Cam Link 4K swap is `CAM  LINK 4K: CAM  LINK 4K` — a DOUBLE space between CAM and
LINK. A bare `lower.contains("cam link")` MISSES it; match on
`lower.split_whitespace().collect::<Vec<_>>().join(" ").contains("cam link")`. When you add a new
`GrabberModel` variant, the compiler forces you through all four exhaustive matches (Display,
`tolerance_pct_for_model`, `sustained_tolerance_pct_for_model`,
`capture.rs::documented_controls_for_model`) — a genuinely stable replacement card (Cam Link 4K:
0.33% spread) gets the STRICT base `CAPTURE_RATE_TOLERANCE_PCT`, not a wide per-model allowance.

**3. Read the CODE + the LIVE fleet before trusting a ticket's "relaxation" claims.** #1034 named
three relaxations; live journals showed two were already resolved: the `#717` sustained band
already escalates to a USB reset via issue-971 (it FIRED on cam1 at 05:36Z that day), and the
offline gate already hard-fails on the `#971` chronic line — folding the plain sustained band in
would recreate the issue-909 mistake (see the issue-992 section above). targets.md also lied
(claimed cam1 = Elgato post-#728); live truth was a ShadowCast. Always re-measure via
`journalctl -u camera-box` on the boxes (`ssh root@10.77.9.6N`, pw newlevel) before pinning a bound.

## #946 + #910 (2026-08-14) — one recognised-event table, and the burn-instance-log restart source

Two reusable findings from bundling capture-wedge/emit-freeze attribution (#946) with the burn-log
source blind spot (#910):

**1. The burn-instance log is journald-BLIND tracing stdout — ANSI-wrapped, microsecond RFC3339-Z.**
During an E2E burn the harness stops `camera-box.service` and runs each camera's capture as a
transient `systemd-run` unit with `StandardOutput=append:/tmp/cbox-burn.log` (source) /
`/tmp/cbox-burn-<cam>.log` (secondary), so journald sees NOTHING in the recording window (the exact
issue-910 blind spot; issue-992 already hit it for capture-rate). A restart event (`#663` reset,
`CRITICAL #945` wedge, `CRITICAL #944` emit-freeze) during the burn lands ONLY in that file. Its
lines are `tracing_subscriber::fmt()` stdout: **ANSI-colour-wrapped, with a microsecond RFC3339-Z
timestamp as the first field** — live-verified shape
`\x1b[2m2026-08-14T10:17:56.523683Z\x1b[0m \x1b[33m WARN\x1b[0m … message` (the message body itself
is ANSI-free, so a substring grep on the event text still matches the raw line). Parse it by
ANSI-stripping (`sed -E 's/\x1b\[[0-9;]*m//g'`), taking the first field, and converting with
`date -u -d "$ts" +%s%N` (TZ-unambiguous via `-u`+`Z`, locale-independent). This parse runs LOCALLY
on dev1 (where the harness parses), never on the remote camera box — only a plain `grep` runs
remotely (`restart_event_burn_log_grep_cmd`). Read BOTH journald AND the burn log for every camera
and `sort -u` (defensive — the two are effectively mutually exclusive during a real burn). See
`restart_events_from_burn_log_output` in `scripts/lib/self-heal-attribution.sh`; the exact-ns
parse is pinned in `tests/harness_self_heal_attribution_895.rs`.

**2. ONE recognised-event table, never N parallel greps — and how to add a FOURTH kind.**
The self-heal reset joined the two watchdog restarts in ONE table (`restart_event_kind_patterns`:
`LABEL<TAB>GREP_PATTERN` rows), keyed on each kind's ONE distinct line (the shared #663 reset line;
each watchdog's uniquely-worded `CRITICAL #NNN` line — NOT an upstream detection band's WARN
wording, the generalisable lesson above). To add a future FOURTH restart kind: add a row to
`restart_event_kind_patterns` (bash) and a variant to `RestartEventKind` (`src/self_heal_attribution.rs`,
with `label`/`from_label`/`detail` arms — the compiler forces you through the matches). The event
correlates through the SAME `attribute_self_heal` engine + ALLOW-not-SUPPRESS gating; only the
`kind` label/message differs. **Design decision (extend, don't fork):** all restart kinds share the
correlation engine, gating, and report shape, so a sibling attribution module would duplicate all
of it — exactly the "competing attribution parser" this rule + the `capture_wedge`/`emit_freeze`
module docs warn against. The CLI token carries an optional kind prefix: `SelfHealResetEvent::parse`
accepts BOTH legacy 2-field `cambox:ns` (→ SelfHealReset, the `--self-heal-reset` back-compat) and
3-field `kind:cambox:ns` (`--restart-event`); the field count is an unambiguous discriminator
because camboxes never contain `:` and the epoch is numeric.

## #994 (2026-08-15) — the capture-rate POST-recording check now sweeps SECONDARY cameras too, REPORT-ONLY

The `[7b/8]` capture-delivery-rate POST-recording check (#705 + its #992 burn-log read) only ever
read the SOURCE camera (`$CAMERA_NAME` / `$CAM1_IP` / `/tmp/cbox-burn.log`). Under `ALL_CAMBOX=1`
every active secondary camera also runs its OWN capture burn (`[2b/8]`, logging to
`/tmp/cbox-burn-<camname>.log`) and is cut into strih program, so a capture-rate defect on a
secondary during the recording (issue 889: cam1 AND cam2 both went over-rate at once — cam2 is a
secondary) was invisible. #994 adds a secondary sweep right AFTER the source-camera check's success
line, looping `CAMBOX_SECONDARY_DEPLOY` the same way the #910 restart-event scan does, reading each
box's journald window + its own burn log, HARD band + #717 SUSTAINED band grepped separately.

**It is REPORT-ONLY (`WARNING #994:`, never aborts) — this was a deliberate architectural decision,
not laziness.** A secondary's capture-RATE band stays report-only because the genlock decimation
gate absorbs a chronic rate wobble (a secondary's `#663` RESET events, a DISTINCT signal, now gate
via `self_heal_reset` since issue 905 — #914 had decoupled them while cam1's grabber was unresolved).
Hard-failing a secondary's capture-rate band here would re-introduce the exact permanently-red-gate
mistake #909/#914 spent three tickets eliminating: cam2 IS a secondary, so a chronic secondary
grabber quirk (the ShadowCast class) would abort every `ALL_CAMBOX` run before a verdict is ever
computed. Per the owner's standing "green gate first, tighten via tickets" directive, the secondary
sweep surfaces the defect loudly for diagnostics and leaves the pass/fail decision to the
source-camera check (still HARD) + the already-report-only verdict terms. A future hard-gate
tightening for secondaries, if ever wanted, is its own ticket. Option 2 of #994 (the reset-EVENT
sweep across secondaries) was ALREADY delivered by #910; #994 closes option 1 for the capture-rate
defect-declaration signal, i.e. the ticket's "both".

**Anchor-safety note (the recurring `recording-e2e.sh` static-anchor gotcha):** the new sweep is
placed AFTER the source-camera check's success line, OUTSIDE the `s[check_header_pos..check_ok_pos]`
region that `harness_capture_rate_guard.rs` measures its `hard_calls == 2` / `sustained_calls == 2`
counts against — so the source block's counts are untouched. The loop var is `_cn_ip_crs` (NOT
`_cn_ip_burn`, which `harness_recording_e2e_paths.rs` slices on, and NOT the `for _hbs in
"${BURN_TARGETS[@]}"` literal it counts == 2), and the new `if [ "${ALL_CAMBOX:-0}" = "1" ]; then`
is followed by an `echo`, never `for _acn in`, so the `#286` ALL_CAMBOX-adjacency `.find()` anchor
is unaffected. Two new pure report-only formatters
(`capture_rate_secondary_recurrence_warn_message` / `..._burn_log_recurrence_warn_message`) mirror
the #992 sustained-band warn formatters' shape.

## The e2e_discord_report.py classifier mirror must key on the GATE set, not the report-only trip set (issue 905 item 2)

When `frozen_leg`/`self_heal_reset` flip report-only↔blocking, `scripts/e2e_discord_report.py` is a
SEPARATE consumer (see `e2e-discord-report.md` / `verdict-gate-seam-calibration.md` §15) whose
`_blocking_failures` branch must key on EXACTLY what folds into `overall_pass` — which is NARROWER
than the report-only trip condition. The naive "mirror the report-only branch" gets it wrong twice:

- `frozen_leg` BLOCKS on `frozen` non-empty ONLY. `stale_replay` NEVER gates (`any_frozen()` reads
  only `self.frozen`), so a blocking branch keyed on `frozen or stale_replay` would wrongly red a
  stale-replay-only run; `stale_replay` stays report-only regardless of the flag.
- `self_heal_reset` BLOCKS on `attributed` OR `unattributed_events` (`any_self_heal()` reads BOTH).
  The pre-905 report-only branch checked only `attributed`, silently missing an unattributed-only
  run — the blocking mirror must check both.

Guard both blocking branches `gates_overall_pass is True` and both report-only branches `is not
True` (the delivery-spread pattern) so a pre/post-flip verdict routes to exactly ONE list (no
double-count). And when a flip moves ONE sub-signal of a COMBINED report-only label to blocking
(e.g. `frozen` out of the old `"zamrznutá/stale vetva"`), SPLIT the label — else the report-only
line echoes a now-blocking term (`zamrznutá`) directly under its own FAIL bullet.

## The FIRST strict run failed on a diffuse-copies shape — `classify_leg`'s conservative branch removed (item 2 follow-up, 2026-09-03)

The item-2 restore above went live and its very FIRST strict E2E run (PR .615, run 33688893588,
`verdict-611325119.json`, RUN_ID 611325119) failed — but not on a real freeze. `frozen_leg.frozen`
fired on CAM2's window since `1788388542251771245`: `copies=18`, `frames=847`, ~30.2s window,
`density≈0.0213`, `approx_stale_secs≈0.64` — BOTH `#758` thresholds (5.0s / 10%) sat FAR under
their bar. The window's own `residual_events` showed all 18 copies as ISOLATED single-frame
duplicates (`kind:"copy"`, `tick_before==tick_after`) spread across the window (offsets 1.2s,
6.07s, 6.5s, 7.57s, 8.34s, 9.47s, 10.9s…) — a diffuse FIFO-jitter signature
(`genlock-fifo-limit-cycle-diagnosis.md`), not a contiguous stuck run — and the SAME window was
already `#889 WITHIN TOLERANCE` under CAM2's own per-cambox `copies_gaps_tolerance` (25, issue
1249): two gates over ONE signal had disagreed.

**Root cause: `frozen_leg::classify_leg` carried a THIRD, conservative branch beyond the two real
`#758` thresholds** — any window whose `copies` count exceeded `STALE_REPLAY_MAX_ISOLATED` (5)
classified `Frozen` even when NEITHER real threshold tripped. Harmless while item 2 was
report-only; the moment item 2 flipped BLOCKING, that branch started failing exactly the diffuse
class the per-cambox tolerance already owns correctly.

**Fix (supervisor design, comment 5517958915, Approach 1 — the other two rejected: a per-cambox
allowance threaded into `classify_leg` couples it to the WALKED tolerance and needs a probe-gated
signature change for no benefit; a contiguity-based freeze metric from `residual_events` is a
bigger root-cause-lane change, deferred to issue 1242):** `classify_leg` is now strictly two-tier —
`Frozen` ⇔ one of the two `#758` thresholds trips; everything else with `copies > 0` is
`StaleReplay { copies }`, regardless of the count. `frozen_leg` now measures EXACTLY what the
`#758` acceptance criteria named (sustained MORE than 5s or MORE than 10% of the window — strict
`>`, exactly-at-the-boundary is still `StaleReplay`, see `frozen_leg.rs`'s own
`exactly_at_both_thresholds_is_not_yet_frozen` test); the diffuse class has
exactly ONE owner again (the per-cambox tolerance). `STALE_REPLAY_MAX_ISOLATED` stays as a
documented, now genuinely UNUSED informational constant (see `frozen_leg.rs`'s own doc). No new
parameters, no probe-gated call-site change — the fix is entirely confined to the one pure
Tier-0 crate-root module (see `frozen_leg.rs`'s own module doc for the full evidence + mechanism).

**Item 2 stays BLOCKING — this is a FIX to the classifier, not a re-decouple.** `.616` = this fix;
if a FUTURE strict run still fails `frozen_leg` on a genuinely diffuse (tolerance-owned) shape, that
is evidence AGAINST this fix and warrants revisiting — but a run that fails on an ACTUAL sustained
freeze (either `#758` threshold genuinely tripped) is item 2 doing its job correctly.

**Generalizable lesson: when TWO gates classify the SAME underlying signal (here: `copies` per
window) via DIFFERENT mechanisms (a fixed constant vs. a walked/calibrated tolerance), a
"conservative, fail-loud" branch in the STRICTER-sounding gate can silently duplicate — and
disagree with — the gate that already owns that signal's real acceptance bar.** The fix is not
"widen the tolerance" or "soften the gate" — it is recognizing that ONE of the two gates should not
be deciding that signal's fate at all once the other genuinely owns it; scope each gate to what its
OWN acceptance criteria actually named, never to "anything left over that feels risky".

**Tier-0 verification technique for a fix that spans TWO sibling crate-root modules — a COMBINED
rustc replica, not two separate ones.** `frozen_leg.rs` and `self_heal_attribution.rs` are both
pure (no probe deps) but `self_heal_attribution.rs` `use crate::frozen_leg::{...}` — a standalone
`rustc --test` replica of EITHER file alone (the existing single-file pattern, see
`verdict-gate-seam-calibration.md` §11) cannot prove the FIX didn't silently break the SIBLING
module's own tests, which is exactly the risk here (6 `self_heal_attribution.rs` fixtures relied
on `frozen_leg`'s removed branch to reach a classified-Frozen window). Concatenate both files as
`pub mod frozen_leg { <frozen_leg.rs verbatim> }` / `pub mod self_heal_attribution { <verbatim,
its `use crate::frozen_leg::...` line resolves unchanged since `crate` = this ONE combined file>
}` into one scratch file, then `rustc --edition 2021 --test <file> -o <bin> && <bin>` — this ran
ALL 35 (later 36) tests from both modules together and genuinely caught nothing broken. Reusable
whenever a fix to a shared pure module has sibling pure-module consumers with their own test
suites — never assume the sibling module's tests are unaffected just because you didn't touch its
file; verify with the combined replica.
