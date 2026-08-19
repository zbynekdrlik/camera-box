#!/usr/bin/env python3
"""#1003 -- MEASUREMENT-WINDOW per-camera equalization profile: the PURE resolver.

The full-path E2E measures per-camera A/V-sync on the strih->stream chain. A persistent
INTER-CAMERA A/V spread (cam1 ~71-80ms above the coupled cam2<->cam3 pair) survives the #940
grid-pin / #1003 history-anchored relock / #1049 convergence fixes because the strih pins in
force during measurement are set by the [4h/8pre] #900 re-anchor from the PREVIEW-transit basis
in phase-sync-last.json (spread ~17ms), while the REAL recording-delivery spread is ~90ms -- the
preview basis under-measures the delivery spread ~5x (#757 Correction 3). A prior LIVE fix
(pre-set deep equalized pins over WS) was REVERTED because the harness itself re-anchors + a
floor gate forbids a deep set, so equalization cannot be a live-pins write; it must live in the
harness measurement window.

This module is the PURE (no I/O, no OBS WebSocket) resolver for that measurement-window profile.
It reads a checked-in profile of MEASURED INPUTS (per-camera production pins + measured delivery
p50 + measured A/V offset, the production stream hold, a chosen common delivery target) and
DERIVES the equalized-deep test pins, the coherent test stream hold, and the coherent
--av-expected-ms -- so the magic outputs (e.g. 90/168/184/789) are never hand-committed as the
primary data and a single-value edit cannot silently break the pins<->hold<->av_expected
coherence (the #1003 design consult's "derive, don't hard-code" point).

It also owns two PURE decisions the harness needs but must never guess in bash:
  * classify_leftover(): baseline-anchored leftover detection at snapshot time -- the biggest
    trap the revert hit (a prior crashed run leaves a test value live, and obs_phase2's
    keep-current-when->=500 heuristic would adopt it as "production"). Given a live value + the
    profile's own test value + the production reference, it says whether the live value is a
    leftover test state that must be restored to the production reference BEFORE snapshotting.
  * staleness_report(): report-only -- after a run, compare the observed per-camera delivery
    against the profile's expectation; a residual > staleness_frames frames means the checked-in
    profile no longer matches reality and should be re-derived (the supervisor's re-derivation
    trigger, never a hard gate).

The OBS-WebSocket apply/verify/restore lives in obs_phase2.py (sharing read_current_latency /
apply_latency and the state-file + teardown path); the harness wiring lives in
scripts/lib/measurement-eq.sh + recording-e2e.sh. This split keeps measurement state entirely
out of the production drift ecosystem (latency-pins-baseline.json / phase-sync-last.json).

CLI (thin wrapper over the pure functions, for the harness):
  python3 scripts/e2e_measurement_pins.py resolve   --profile <path>              # -> JSON plan
  python3 scripts/e2e_measurement_pins.py staleness --profile <path> --observed <path>

Every function here is stdlib-only and Tier-0 unit-testable (tests/python/
test_e2e_measurement_pins.py) -- no cargo, no rig, no WS.
"""
from __future__ import annotations

import argparse
import json
import os
import sys

# 30fps canvas -- the strih/stream program grid the recording-verdict scores against.
FRAME_PERIOD_MS = 1000.0 / 30.0

# #1003 FRAME-GRID PHASE constraint (2026-08-19 live validation, verdict 1804432786): a pin whose
# release phase frac(pin/frame) < 0.5 sits in the #998/#1049 FIFO limit-cycle-prone band (the
# round-to-nearest target rounds DOWN and undershoots the natural hold -> copies≈gaps churn per
# segment). The live run reproduced it EXACTLY at cam2 pin 168 (frac 0.04): seg copies≈gaps 5/4,
# 7/7, 5/4. So after delivery-equalizing, snap a prone pin to the nearest integer pin whose frac is
# in a ROBUST CENTRE band -- clear of BOTH the 0.5 round-down cliff AND the 1.0 wrap (an NTP step
# storm smears timecode phase fleet-wide, camera-box#1130, so a thin margin above 0.5 is not enough;
# a value near 1.0 can wrap into the prone band under a step). Delivery-equality is the SECONDARY
# term: only prone pins move, to the nearest safe value (cam2 168->160 costs 8ms, keeps equality).
PHASE_PRONE_MAX_FRAC = 0.5   # frac(pin/frame) < this = limit-cycle-prone; a pin at/above it is left alone
PHASE_SAFE_LO_FRAC = 0.6     # snap a prone pin to a frac in [LO, HI] -- centred, margin from both edges
PHASE_SAFE_HI_FRAC = 0.8
PHASE_SNAP_MAX_COST_MS = 20  # a prone pin must find a safe pin within this many ms (else INCOHERENT)


def _phase_frac(pin_ms) -> float:
    """The release-phase fraction frac(pin/frame_period) in [0, 1)."""
    return (float(pin_ms) / FRAME_PERIOD_MS) % 1.0


def _phase_is_prone(pin_ms) -> bool:
    """True iff `pin_ms`'s release phase is in the #998/#1049 FIFO limit-cycle-prone band."""
    return _phase_frac(pin_ms) < PHASE_PRONE_MAX_FRAC


def phase_snap_pin(equalized_ms: float) -> int:
    """PURE: the phase-safe integer pin for a real-valued equalized (delivery-equal) pin.

    `round()` it; if that is NOT prone (frac >= 0.5, round-up overshoot = safe), keep it. Otherwise
    return the NEAREST integer pin whose frac is in the robust centre band [LO, HI], searched
    outward up to PHASE_SNAP_MAX_COST_MS (checking the lower candidate first at each distance, so a
    tie resolves toward LESS latency -- cam2 168 -> 160, not 176). Returns the still-prone rounded
    pin if none is found in range; coherence_check flags that so a prone pin never silently ships."""
    r = int(round(equalized_ms))
    if not _phase_is_prone(r):
        return r
    for d in range(1, int(PHASE_SNAP_MAX_COST_MS) + 1):
        for cand in (r - d, r + d):  # lower-first: a tie resolves toward less latency
            if PHASE_SAFE_LO_FRAC <= _phase_frac(cand) <= PHASE_SAFE_HI_FRAC:
                return cand
    return r


def load_profile(path: str) -> dict:
    """Load + structurally validate the measurement-eq profile JSON.

    Fails LOUD (SystemExit) on a missing file, malformed JSON, or a profile missing any
    required key -- a half-specified profile must never silently resolve to partial pins."""
    if not os.path.exists(path):
        raise SystemExit(f"[measurement-eq] profile not found: {path}")
    try:
        with open(path, encoding="utf-8") as fh:
            prof = json.load(fh)
    except (OSError, ValueError) as exc:
        raise SystemExit(f"[measurement-eq] cannot read profile {path}: {exc}")
    if not isinstance(prof, dict):
        raise SystemExit(f"[measurement-eq] profile {path} is not a JSON object")

    for key in ("target_delivery_ms", "min_deep_pin_ms", "cameras", "stream", "av_expected_ms"):
        if key not in prof:
            raise SystemExit(f"[measurement-eq] profile {path} missing required key: {key!r}")
    cams = prof["cameras"]
    if not isinstance(cams, dict) or not cams:
        raise SystemExit(f"[measurement-eq] profile {path} 'cameras' must be a non-empty object")
    for src, c in cams.items():
        for k in ("production_pin_ms", "production_delivery_p50_ms", "production_av_offset_ms"):
            if k not in c:
                raise SystemExit(
                    f"[measurement-eq] profile {path} camera {src!r} missing {k!r}")
    stream = prof["stream"]
    for k in ("source", "production_hold_ms"):
        if k not in stream:
            raise SystemExit(f"[measurement-eq] profile {path} 'stream' missing {k!r}")
    return prof


def transport_ms(cam: dict) -> float:
    """The pin-INDEPENDENT cam->strih transport = measured delivery p50 - the pin it was
    measured at. This is what stays (roughly) constant across pin changes; the equalized pin is
    chosen against it."""
    return float(cam["production_delivery_p50_ms"]) - float(cam["production_pin_ms"])


def equalized_pin_ms(profile: dict, cam: dict) -> float:
    """The real-valued delivery-EQUALIZED pin (before the frame-grid phase snap): `target -
    transport`, so pin + transport == target for every camera. The PRIMARY objective."""
    return float(profile["target_delivery_ms"]) - transport_ms(cam)


def resolve_pins(profile: dict) -> dict:
    """PURE: derive the delivery-equalized-deep per-camera test pins, THEN frame-grid phase-snap.

    Step 1 (equality, PRIMARY): each camera's (transport + pin) is driven to the SAME
    `target_delivery_ms` so all cameras deliver together and the inter-camera A/V spread collapses.
    The target is chosen so even the smallest (slowest-transport) pin sits at/above `min_deep_pin_ms`.
    Step 2 (phase-safety, overriding): `phase_snap_pin` moves any pin whose release phase is in the
    #998/#1049 FIFO limit-cycle-prone band (frac<0.5) to the nearest safe pin -- the 2026-08-19 live
    validation exposed cam2 pin 168 (frac 0.04) churning copies≈gaps, so phase-safety overrides
    exact equality by up to PHASE_SNAP_MAX_COST_MS. Returns {source: pin_ms(int)}.
    """
    return {
        src: phase_snap_pin(equalized_pin_ms(profile, cam))
        for src, cam in profile["cameras"].items()
    }


def _expected_deliveries(profile: dict) -> dict:
    """Per-camera expected delivery under the RESOLVED (phase-snapped) pins: `snapped_pin +
    transport`. After phase-snapping these are no longer all == target (a prone pin was moved off
    the grid), so the hold, the coherence A/V check, and staleness all key on THESE, not on target."""
    pins = resolve_pins(profile)
    return {src: pins[src] + transport_ms(cam) for src, cam in profile["cameras"].items()}


def _mean_audio_ref_ms(profile: dict) -> float:
    """The common audio reference: the delivery level that maps to A/V offset 0 under the
    PRODUCTION hold. Per camera it is `delivery - av_offset`; the profile carries a few cameras'
    worth of the SAME physical audio path, so the mean is the robust single value."""
    refs = [
        float(c["production_delivery_p50_ms"]) - float(c["production_av_offset_ms"])
        for c in profile["cameras"].values()
    ]
    return sum(refs) / len(refs)


def resolve_hold(profile: dict) -> int:
    """PURE: derive the coherent stream test hold.

    Equalizing delivery raises every camera's A/V offset to a common level under the production
    hold; lowering the stream hold by `(common_level - av_expected)` re-zeroes it to `av_expected`.
    Because the frame-grid phase snap leaves per-camera deliveries slightly UNEQUAL, the hold centres
    on the MEAN snapped delivery (not `target`), so the MEAN A/V lands at av_expected with only the
    small per-camera snap residual as spread:
        test_hold = prod_hold - (mean_snapped_delivery - mean_audio_ref - av_expected)
    Returns an int (genlock_latency_ms_src is an integer OBS setting).
    """
    av_expected = float(profile["av_expected_ms"])
    audio_ref = _mean_audio_ref_ms(profile)
    prod_hold = float(profile["stream"]["production_hold_ms"])
    deliveries = _expected_deliveries(profile)
    mean_delivery = sum(deliveries.values()) / len(deliveries)
    common_level = mean_delivery - audio_ref
    return int(round(prod_hold - (common_level - av_expected)))


def resolve_av_expected(profile: dict) -> float:
    """PURE: the --av-expected-ms the A/V gate must expect given the pin+hold design. With the
    hold rebalanced to re-zero the common level this is the profile's `av_expected_ms` (0), NOT
    an inherited blind 0 -- the coherence test proves the pins+hold actually produce it."""
    return float(profile["av_expected_ms"])


def resolve_plan(profile: dict) -> dict:
    """The full resolved plan the harness applies: strih pins, stream hold, av_expected, and the
    stream source name. Includes the production references so the apply step can do
    baseline-anchored leftover detection without re-reading the profile."""
    return {
        "strih_pins": resolve_pins(profile),
        "stream_source": profile["stream"]["source"],
        "stream_hold_ms": resolve_hold(profile),
        "av_expected_ms": resolve_av_expected(profile),
        "production": {
            "strih_pins": {
                src: int(c["production_pin_ms"]) for src, c in profile["cameras"].items()
            },
            "stream_hold_ms": int(profile["stream"]["production_hold_ms"]),
        },
    }


def coherence_check(profile: dict) -> list:
    """PURE: return a list of coherence-violation strings (empty == coherent), caught at Tier-0
    rather than on the rig:
      1. every derived pin >= min_deep_pin_ms (deep-phase regime);
      2. PHASE-SAFETY: no derived pin sits in the FIFO limit-cycle-prone band (frac<0.5) -- fires
         only when a prone equalized pin had NO safe pin within PHASE_SNAP_MAX_COST_MS;
      3. PHASE-SNAP COST: each pin is within PHASE_SNAP_MAX_COST_MS of its equalized value (the snap
         did not wander far from delivery-equality);
      4. the predicted per-camera A/V under the SNAPPED delivery + the mean-centred hold is within a
         small band of av_expected (the pins<->hold<->av_expected triple is self-consistent).
    """
    problems = []
    min_deep = float(profile["min_deep_pin_ms"])
    av_expected = float(profile["av_expected_ms"])
    pins = resolve_pins(profile)
    hold = resolve_hold(profile)
    prod_hold = float(profile["stream"]["production_hold_ms"])
    hold_drop = prod_hold - hold

    for src, cam in profile["cameras"].items():
        pin = pins[src]
        if pin < min_deep:
            problems.append(
                f"{src}: derived pin {pin} < min_deep_pin_ms {min_deep:g} "
                f"(not in the deep-phase regime)")
        # Invariant 2 -- phase-safety: resolve_pins snaps a prone equalized pin to a safe one, so
        # this only fires when NO safe pin existed within PHASE_SNAP_MAX_COST_MS (phase_snap_pin
        # returned the still-prone rounded value). That is a genuine "cannot equalize AND phase-fix
        # this camera within budget" state -- surface it, never ship a prone pin.
        if _phase_is_prone(pin):
            problems.append(
                f"{src}: derived pin {pin} frac {_phase_frac(pin):.2f} < {PHASE_PRONE_MAX_FRAC:g} "
                f"-- FIFO limit-cycle-prone (no safe pin within {PHASE_SNAP_MAX_COST_MS:g}ms of the "
                f"equalized value; widen the profile target or investigate the transport)")
        # Invariant 3 -- phase-snap cost bounded.
        equalized = equalized_pin_ms(profile, cam)
        if abs(pin - equalized) > PHASE_SNAP_MAX_COST_MS:
            problems.append(
                f"{src}: phase-snap moved the pin {abs(pin - equalized):.1f}ms off the equalized "
                f"value {equalized:.1f} (> {PHASE_SNAP_MAX_COST_MS:g}ms budget)")
        # Invariant 4 (genuinely falsifiable): the cameras share ONE physical audio path, so each
        # camera's audio reference (delivery - av_offset) should be ~equal; the mean-centred hold can
        # only re-zero all of them to av_expected if that spread (plus the per-camera snap residual)
        # is small. 8ms (~1/4 frame) tolerates ordinary re-measurement noise + the phase-snap
        # residual while catching a profile whose per-camera A/V offsets are inconsistent.
        audio_ref_i = float(cam["production_delivery_p50_ms"]) - float(cam["production_av_offset_ms"])
        predicted_av = (pin + transport_ms(cam)) - audio_ref_i - hold_drop
        if abs(predicted_av - av_expected) > 8.0:
            problems.append(
                f"{src}: predicted equalized A/V {predicted_av:.1f} not within 8ms of "
                f"av_expected {av_expected:g} (inconsistent per-camera audio refs or excess snap)")
    return problems


def classify_leftover(live_ms, production_ref_ms, test_value_ms, slack_ms: float) -> str:
    """PURE: at snapshot time, is `live_ms` a genuine production value to snapshot, a leftover test
    state a prior crashed run left behind, or a value the profile no longer agrees with?

    Returns one of:
      "snapshot"        -- live matches the production reference (within slack) -> snapshot it as-is.
      "leftover-test"   -- live EQUALS the profile's own test value -> a prior crashed run left the
                           test value in force. CERTAIN inference: restore the production reference
                           FIRST (loud), then snapshot THAT, so a stuck-test run can never perpetuate.
      "stale"           -- live is beyond `slack_ms` of the production reference AND is not the test
                           value -> the LIVE RIG DISAGREES with the profile (the profile's production
                           reference is stale, or the rig legitimately re-tuned). This is a GUESS,
                           not a certainty, so the caller must FAIL LOUD and never auto-write a
                           checked-in constant over the live value (the 2026-08-19 revert incident:
                           the stream hold is an operator-retunable value; silently restoring it to a
                           file constant and leaving it there is exactly the stomp that was reverted).
      "unknown"         -- live could not be read (None) -> caller decides (never treated as prod).

    The KEY distinction from the naive earlier form: "beyond slack" is split from "equals the test
    value". Only the latter is a certain leftover to auto-recover; the former is a stale-profile /
    rig-drift signal to REFUSE on, never to overwrite from the profile."""
    if live_ms is None:
        return "unknown"
    live = float(live_ms)
    if test_value_ms is not None and abs(live - float(test_value_ms)) < 0.5:
        return "leftover-test"
    if production_ref_ms is None or abs(live - float(production_ref_ms)) <= float(slack_ms):
        return "snapshot"
    return "stale"


def staleness_report(profile: dict, observed_delivery_ms: dict, staleness_frames: float) -> dict:
    """PURE, REPORT-ONLY: after a profile-mode run, does the checked-in profile still match reality?

    `observed_delivery_ms` maps each camera source -> the delivery p50 the verdict actually
    measured (WITH the test pins in force). Under the profile every camera should deliver at
    `target_delivery_ms`; a residual > staleness_frames * FRAME_PERIOD_MS means the physical
    transports have drifted and the profile should be RE-DERIVED. Returns
    {stale: bool, threshold_ms: float, cameras: {src: {observed, expected, residual, stale}}}.
    Never raises on a camera missing from `observed_delivery_ms` (it is simply skipped -- a
    partial verdict is not evidence of staleness)."""
    # Key on the per-camera EXPECTED delivery under the RESOLVED (phase-snapped) pins, not `target`
    # -- after the frame-grid snap a camera's expected delivery is snapped_pin + transport, which
    # need not equal target (cam2 168->160 -> expected ~198.5, not 207).
    expected = _expected_deliveries(profile)
    threshold = staleness_frames * FRAME_PERIOD_MS
    cams = {}
    any_stale = False
    for src in profile["cameras"]:
        if src not in observed_delivery_ms or observed_delivery_ms[src] is None:
            continue
        observed = float(observed_delivery_ms[src])
        exp = expected[src]
        residual = abs(observed - exp)
        stale = residual > threshold
        any_stale = any_stale or stale
        cams[src] = {
            "observed_ms": round(observed, 1),
            "expected_ms": round(exp, 1),
            "residual_ms": round(residual, 1),
            "stale": stale,
        }
    return {"stale": any_stale, "threshold_ms": round(threshold, 1), "cameras": cams}


def _cmd_resolve(args) -> int:
    profile = load_profile(args.profile)
    problems = coherence_check(profile)
    if problems:
        sys.stderr.write(
            "[measurement-eq] profile INCOHERENT -- refusing to resolve:\n  "
            + "\n  ".join(problems) + "\n")
        return 1
    print(json.dumps(resolve_plan(profile), indent=2, sort_keys=True))
    return 0


def _cmd_staleness(args) -> int:
    profile = load_profile(args.profile)
    with open(args.observed, encoding="utf-8") as fh:
        observed = json.load(fh)
    frames = args.staleness_frames
    if frames is None:
        frames = float(profile.get("staleness_frames", 1.5))
    report = staleness_report(profile, observed, frames)
    print(json.dumps(report, indent=2, sort_keys=True))
    if report["stale"]:
        sys.stderr.write(
            "[measurement-eq] measurement profile STALE -- observed delivery drifted "
            f"> {report['threshold_ms']}ms from the equalization target; re-derive "
            f"{args.profile} from a fresh delivery measurement.\n")
    return 0  # report-only: never fails the caller


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("resolve", help="print the resolved pins/hold/av_expected plan as JSON")
    r.add_argument("--profile", required=True)
    r.set_defaults(func=_cmd_resolve)

    s = sub.add_parser("staleness", help="report-only: is the profile still current vs observed delivery")
    s.add_argument("--profile", required=True)
    s.add_argument("--observed", required=True,
                   help="JSON {source: observed_delivery_p50_ms} from the run's verdict")
    s.add_argument("--staleness-frames", type=float, default=None)
    s.set_defaults(func=_cmd_staleness)

    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
