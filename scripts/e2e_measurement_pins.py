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


def resolve_pins(profile: dict) -> dict:
    """PURE: derive the delivery-equalized-deep per-camera test pins.

    Every camera's (transport + pin) is driven to the SAME `target_delivery_ms`, so all cameras
    deliver at the same instant and the inter-camera A/V spread collapses. The slowest-transport
    camera gets the smallest pin; the target is chosen (in the profile) so even that smallest pin
    sits at/above `min_deep_pin_ms` -- deep enough to be out of the shallow phase-churn regime
    the #1003 finding names. Returns {source: pin_ms(int)}.
    """
    target = float(profile["target_delivery_ms"])
    return {
        src: int(round(target - transport_ms(cam)))
        for src, cam in profile["cameras"].items()
    }


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

    Equalizing delivery to `target_delivery_ms` raises every camera's A/V offset to a common
    level `target - audio_ref` under the production hold. Lowering the stream hold by
    `(common_level - av_expected)` re-zeroes it to `av_expected`. So:
        test_hold = prod_hold - (target - audio_ref - av_expected)
    Returns an int (genlock_latency_ms_src is an integer OBS setting).
    """
    target = float(profile["target_delivery_ms"])
    av_expected = float(profile["av_expected_ms"])
    audio_ref = _mean_audio_ref_ms(profile)
    prod_hold = float(profile["stream"]["production_hold_ms"])
    common_level = target - audio_ref
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
    """PURE: return a list of coherence-violation strings (empty == coherent). The invariants the
    #1003 consult mandated, so a bad profile edit is caught at Tier-0 rather than on the rig:
      1. every derived pin >= min_deep_pin_ms (deep-phase regime);
      2. pin_i + transport_i == target for every camera (delivery equalized, +/-1ms rounding);
      3. the predicted equalized A/V per camera under the derived hold is within a small band of
         av_expected (the pins<->hold<->av_expected triple is self-consistent).
    """
    problems = []
    target = float(profile["target_delivery_ms"])
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
        eq_delivery = pin + transport_ms(cam)
        if abs(eq_delivery - target) > 1.0:
            problems.append(
                f"{src}: pin+transport {eq_delivery:.1f} != target {target:g} "
                f"(delivery not equalized)")
        audio_ref_i = float(cam["production_delivery_p50_ms"]) - float(cam["production_av_offset_ms"])
        predicted_av = target - audio_ref_i - hold_drop
        if abs(predicted_av - av_expected) > 5.0:
            problems.append(
                f"{src}: predicted equalized A/V {predicted_av:.1f} not within 5ms of "
                f"av_expected {av_expected:g}")
    return problems


def classify_leftover(live_ms, production_ref_ms, test_value_ms, slack_ms: float) -> str:
    """PURE: at snapshot time, is `live_ms` a genuine production value to snapshot, or a leftover
    test state a prior crashed run left behind?

    Returns one of:
      "snapshot"        -- live matches the production reference (within slack) -> snapshot it as-is.
      "leftover-test"   -- live equals the profile's test value, OR deviates from the production
                           reference beyond slack -> restore the production reference FIRST (loud),
                           then snapshot THAT, so a stuck-production run can never perpetuate.
      "unknown"         -- live could not be read (None) -> caller decides (never treated as prod).

    This kills the stuck-production incident class the 2026-08-19 revert hit head-on: the harness
    never adopts a leftover 789 (or deep strih pin) as production."""
    if live_ms is None:
        return "unknown"
    live = float(live_ms)
    if test_value_ms is not None and abs(live - float(test_value_ms)) < 0.5:
        return "leftover-test"
    if production_ref_ms is not None and abs(live - float(production_ref_ms)) > float(slack_ms):
        return "leftover-test"
    return "snapshot"


def staleness_report(profile: dict, observed_delivery_ms: dict, staleness_frames: float) -> dict:
    """PURE, REPORT-ONLY: after a profile-mode run, does the checked-in profile still match reality?

    `observed_delivery_ms` maps each camera source -> the delivery p50 the verdict actually
    measured (WITH the test pins in force). Under the profile every camera should deliver at
    `target_delivery_ms`; a residual > staleness_frames * FRAME_PERIOD_MS means the physical
    transports have drifted and the profile should be RE-DERIVED. Returns
    {stale: bool, threshold_ms: float, cameras: {src: {observed, expected, residual, stale}}}.
    Never raises on a camera missing from `observed_delivery_ms` (it is simply skipped -- a
    partial verdict is not evidence of staleness)."""
    target = float(profile["target_delivery_ms"])
    threshold = staleness_frames * FRAME_PERIOD_MS
    cams = {}
    any_stale = False
    for src in profile["cameras"]:
        if src not in observed_delivery_ms or observed_delivery_ms[src] is None:
            continue
        observed = float(observed_delivery_ms[src])
        residual = abs(observed - target)
        stale = residual > threshold
        any_stale = any_stale or stale
        cams[src] = {
            "observed_ms": round(observed, 1),
            "expected_ms": round(target, 1),
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
