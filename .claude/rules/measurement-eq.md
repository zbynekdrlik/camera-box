---
paths:
  - "scripts/e2e_measurement_pins.py"
  - "scripts/lib/measurement-eq.sh"
  - "scripts/e2e-measurement-pins.json"
  - "tests/python/test_e2e_measurement_pins.py"
  - "tests/python/test_obs_phase2_measurement_pins_1003.py"
---

# Measurement-window per-camera A/V equalization (#1003, `MEASUREMENT_EQ`)

> **SUPERSEDED as the production-alignment path (#1003 owner rework, 2026-08-20).** The
> DELIVERY-equalized DEEP pins this profile derives (`resolve_pins` → 90/160/184, `resolve_hold` →
> 791) were PROMOTED to production, then REJECTED by the owner and REVERTED (`0aaa2fc93`): absolute
> deep pins add ~180 ms of needless chain latency, and the single delivery-p50 sample baked in a
> degraded cam1 grabber. Production alignment is now the per-run **FLOOR-3 auto-align**
> (`scripts/qr_align_pins.py`, wired as `recording-e2e.sh`'s BLOCKING `[4i/8align]` step): the
> slowest camera → pin 3, others → 3 + their RELATIVE delta from the exact simultaneous painter-QR
> `gen_ts_ns` difference (medianed, underrun-excluded), covering ALL on-air cameras incl. cam4
> (`CAMERA_ALIGN_SET`), verified by re-measuring the `frame_id` spread ≤ 1. See
> `.claude/rules/latency-pins-verify.md` → "Production alignment = the per-run FLOOR-3 auto-align".
> This `MEASUREMENT_EQ` profile REMAINS a valid OPT-IN experiment (its own report-only diagnostics
> unaffected) and is MUTUALLY EXCLUSIVE with the `[4i/8align]` step in one run (both write strih
> pins), but it is NO LONGER the model for setting production alignment pins. Do NOT re-promote its
> deep output to the committed baseline. Do NOT hand-bake floor-3 numbers into the profile either —
> the floor-3 pins are re-derived LIVE per run, never committed.

An OPT-IN (`MEASUREMENT_EQ=1`, default OFF) harness mode that, during an ALL_CAMBOX E2E run, applies
delivery-equalized-deep strih per-camera pins + a coherent stream hold FOR THE MEASUREMENT WINDOW
ONLY (snapshot-set-restore), so the inter-camera A/V spread collapses in the measurement without
touching production (3/6/20 + 971, which stay the drift-guard source of truth in
`scripts/latency-pins-baseline.json`). Ships opt-in pending live E2E validation — the supervisor
enables it for the validation run; promotion to default-ON is #1003's own progression (like the
#757→#900 auto-pin/re-anchor progression), not a new ticket.

## Why the #900 re-anchor's basis is WRONG for A/V equalization (the core insight)
The `[4h/8pre]` #900 re-anchor equalizes on PREVIEW transits (`~/.camera-box/phase-sync-last.json`,
spread ~17ms). The real recording DELIVERY spread is ~90ms — preview under-measures ~5× (#757
Correction 3). So the phase-sync pins leave cam1 ~73ms displaced in the A/V gate. Equalization for
the A/V gate MUST be DELIVERY-derived (from a recording measurement), never preview/re-anchor-derived.
This is why it is a SEPARATE profile, not a floor/margin bolted onto the re-anchor.

## The moving parts (all mutually consistent — never wire one without the others)
- **Config `scripts/e2e-measurement-pins.json`**: stores MEASURED inputs (per-cam production pin +
  measured delivery p50 + measured A/V offset, production stream hold, common delivery target), NOT
  the magic outputs. `scripts/e2e_measurement_pins.py` (PURE, Tier-0) DERIVES pins/hold/av-expected
  in TWO steps: (1) equalize `eq_i=target−transport_i`; (2) FRAME-GRID PHASE snap (below). The hold
  then `= prod_hold − (mean_SNAPPED_delivery − mean_audio_ref − av_expected)` (centres the MEAN
  snapped delivery, since the snap leaves deliveries slightly unequal). Coherence check + re-derive
  per the config's own `_comment` after a staleness signal.
- **FRAME-GRID PHASE constraint (`phase_snap_pin`, 2026-08-19 live validation, verdict 1804432786)**:
  the equalization worked (delivery spread 81-94ms→3.67ms, A/V uniform-pass) but exposed cam2 at pin
  168 (frac(168/33.33)=0.04) hitting the #998/#1049 FIFO limit-cycle-prone band — `frac(pin/frame)<0.5`
  → the round-to-nearest release target rounds DOWN and undershoots → copies≈gaps churn per segment
  (see `.claude/rules/genlock-fifo-limit-cycle-diagnosis.md`). So after equalizing, snap any prone pin
  to the nearest integer pin whose frac is in the robust CENTRE band `[0.6, 0.8]` (clear of BOTH the
  0.5 round-down cliff AND the 1.0 wrap — an NTP step storm smears phase fleet-wide, camera-box#1130).
  cam2 168→160 (frac 0.80, 8ms cost); cam1 90 (0.70) + cam3 184 (0.52) already safe (frac≥0.5, kept).
  Phase-safety OVERRIDES exact equality (secondary term) by up to `PHASE_SNAP_MAX_COST_MS`=20; the
  hold re-centring keeps the residual spread ~4ms. A pin at frac≥0.5 (round-UP overshoot) is safe and
  left alone — do NOT snap a merely-borderline-clean pin (adds spread for nothing).
- **#900 re-anchor + #893 floor gate**: `MEASUREMENT_EQ=1` FORCES `PHASE_REANCHOR=0` (one flag, they
  can never disagree — both write strih pins) and the `[4h/8]` #893 floor gate is SKIPPED (the deep
  pins deliberately violate its min==3ms invariant); it is REPLACED by `obs_phase2.py
  verify-measurement-pins` (a pre-record read-back that the intended profile values are actually in
  force — catches a surviving writer / failed apply / wrong input name).
- **snapshot-restore rides the EXISTING teardown path**: `apply-measurement-pins` snapshots the
  production pins into the obs_phase2 state file; `teardown --host STRIH` (already called from
  recording-e2e.sh cleanup) restores them via `_restore_measurement_pins`. The stream hold rides the
  existing `_snapshot_and_set_test_latency`/`_restore_test_latency` (#358/#691) path.
- **The live cam→strih p99 gate is SEPARATE and pin-dependent**: raising cam2's (the marker camera's)
  pin +N raises `latency.cam_strih` +N, so profile mode MUST raise `--max-cam-strih-p99-latency-ms`
  by the cam2 pin delta (`measurement_eq_cam_strih_bound_ms`), or that 400ms gate false-fails by
  construction. It is a DIFFERENT gate from the A/V-offset gate — remember to adjust it too.

## Gotchas that cost time here
- **`classify_leftover` is THREE-way, on purpose.** `live==test value` → auto-restore (a crashed
  prior run left the test value — certain). `live≈production ref` → snapshot. **Beyond slack AND not
  the test value → `stale` → FAIL LOUD, NEVER auto-write a checked-in constant over the live value.**
  The stream hold is operator-retunable (its drift baseline is 915±60), so silently restoring it to
  the profile's 971 and leaving it there is exactly the 2026-08-19 revert incident. Do not collapse
  the stale branch back into leftover-test.
- **`_restore_measurement_pins` KEEPS the snapshot on a read-back mismatch** (durable retry artifact).
- **The pin→delivery relationship is NOT simple / not run-stable.** cam1 is bimodal (issue 1033/909:
  ~47-64ms healthy vs ~88-144ms degraded grabber), and #757 per-restart re-phasing swings delivery
  ±1 frame/run — so a STATIC profile is accurate only to ~±1 frame per camera per run. Fine under
  ±90ms; it CANNOT support the ±20ms re-tighten (acceptance item 2) — that follow-up must re-derive.
- **A JSON `_comment` caveat must not contain a literal `"`** — it closes the string and breaks the
  file (`json.load` "Expecting ',' delimiter"). Verify with `python3 -c 'import json; json.load(...)'`
  after any comment edit.
- **Tier-0 (worktree): all cargo is blocked (even `--no-run`).** Verify the python side with
  `pytest tests/python/...`, the bash with `bash -n` + `shellcheck -x`, and the Rust static-anchor
  tests by REPLICATING their `.find`/`.contains`/byte-window assertions in a `python3` script against
  the edited `recording-e2e.sh` (the 900/893/paths/691 + report-only-`exit 1`-window tests).

## Report-only diagnostics on top of the profile (#1124)

Three REPORT-ONLY diagnostics ride the profile — NONE gates (`gates_overall_pass()` / `$GATE`
untouched; data-gated flips have their own tickets). All HARNESS-side (pure python in
`e2e_measurement_pins.py` + sourced-lib wiring in `measurement-eq.sh`), NOT in probe-gated
recording-verdict.rs — recording-verdict has no profile-mode context and the signals are fully
derivable from the verdict JSON the harness already holds post-merge.

- **Staleness (`staleness-from-verdict` CLI)** — `observed_delivery_from_verdict()` maps the
  verdict `all_cambox_delivery_latency` onto the profile keys, then runs the existing
  `staleness_report`. Wired post-verdict (always). Surfaces "profile STALE — re-derive".
- **Edge-oscillation (`edge-oscillation` CLI)** — `edge_oscillation_report()` reads
  `all_cambox_continuity.segments`; a per-cambox window is EDGE-OSCILLATING iff
  `min(copies,gaps)>=3 AND max<=25 AND |c-g|<=0.5*max`; a cambox is a SUSPECT iff `>=2` such
  windows AND ZERO storm windows (`max>25` = a FROZEN leg, a DIFFERENT class — never masked as
  "rerun the edge"). Wired post-verdict ONLY on a FAILED run (`$GATE!=0`). Thresholds are
  DATA-calibrated from 19 local verdict JSONs: fires on EXACTLY the FIFO run 1804432786 (CAM2 pin
  168 frac 0.04, per-seg 5/4,7/7,5/4) and no other (not the healthy post-snap 66065064, not the
  frozen-storm 547108056). Re-calibrate against `/tmp/recording-e2e-*/verdict-*.json`, never a feel.
- **Post-record stomp re-check** — reuses `obs_phase2.py verify-measurement-pins --role
  strih|stream` right after StopRecord (pins still in force; teardown restores only at cleanup exit).

### Gotchas
- **Verdict cam keys are BARE `camN`; the profile keys are `NDI camN`.** `all_cambox_delivery_latency`
  and `all_cambox_continuity.segments[].cambox` (uppercase `CAM1`) never carry the `NDI ` prefix.
  `observed_delivery_from_verdict` maps profile→verdict via `src.split()[-1]`. Any NEW verdict-JSON
  consumer here must remap; a `null` / absent / non-dict delivery entry is SKIPPED (a partial
  verdict is not staleness evidence), never zeroed.
- **A report-only helper called as a BARE statement MUST return 0 on every path (#1133).** The
  post-verdict call site is under the re-enabled `set -euo pipefail`; the post-record one is in the
  `[7/8]` `set +e` region. Both helpers end with an explicit `return 0` AND both call sites carry
  `|| true` (belt-and-suspenders) so a benign non-zero (an empty read, a `python|sed`/pipefail
  hiccup, a `verify-measurement-pins` mismatch) can never `set -e`-abort the run before `exit $GATE`.
  RUNTIME-verify by sourcing the lib under `set -euo pipefail` and calling each helper on its
  failure paths — a `bash -n` / static-anchor test cannot catch this class.
