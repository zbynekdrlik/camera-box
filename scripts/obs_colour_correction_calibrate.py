#!/usr/bin/env python3
"""#738 — OBS-side per-input colour correction for the Elgato 4K S tint (cam1/cam6), calibrated
against a grey-world assumption + cross-checked against cam5's (ShadowCast) near-neutral cast.

## Why grey-world, not a literal cam5-frame match

The issue text says "iterate filter params against cam5's rendition of the same splitter content
as reference" — the SAME-splitter-content setup (`rig-mode.sh test`) requires touching cam2 to
paint a shared reference pattern through the HDMI splitter, which is explicitly OFF-LIMITS for
this work (#737, cam2's dying disk). Live sampling confirmed cam5 and cam1/cam6 are currently
pointed at genuinely DIFFERENT real-world scenes (cam5 near-black, cam1/cam6 a dim room) — a
direct numeric match against cam5's CURRENT frame would be comparing apples to oranges, not a
colour defect.

Instead this uses the standard "grey-world" white-balance assumption (a large, real, mixed scene
averages close to neutral grey) to compute a PER-CHANNEL gain (`color_multiply` on OBS's
`color_filter_v2` — a genuine independent R/G/B multiplicative gain, mathematically the closest
thing to a white-balance correction the filter exposes, and strictly MORE POWERFUL than V4L2's
saturation/contrast/hue -- see vendor/obs-studio/plugins/obs-filters/color-correction-filter.c:
`filter->color_matrix.{x.x,y.y,z.z} = color_multiply_v4.{x,y,z}` are set INDEPENDENTLY per
channel, unlike a uniform saturation scale which shrinks all three channels' deviation from luma
by the SAME factor). cam5's OWN near-neutral cast (measured separately, on ITS OWN scene) is used
only as a SANITY reference for "what does an uncorrected, undamaged camera's cast look like",
never as the literal target frame.

## Pure functions (unit-tested, tests/python/test_obs_colour_correction_calibrate.py)

- `chroma_cast_bt601`: BT.601 RGB->Cb/Cr chroma-cast metric (the SAME metric family
  camera-box's own `mean_chroma` uses in the V4L2 YUV domain -- src/capture.rs -- just computed
  here in the RGB domain OBS screenshots are already in).
- `grey_world_gains`: the damped per-channel correction gain (`damping=1.0` = full grey-world
  correction, `0.0` = no-op) -- damping is conservative BY DEFAULT since a single/few-sample
  average can still be biased by real scene content, unlike a proper multi-second temporal AWB.
- `pack_color_multiply` / `unpack_color_multiply`: OBS's `color` int encoding for a NON-alpha
  color picker (`obs_properties_add_color`, not `_alpha`) -- byte layout confirmed against a
  live round-trip (SetSourceFilterSettings -> GetSourceFilter) on strih, 2026-07-13:
  `int_value = (0 << 24) | (B << 16) | (G << 8) | R`, each channel `round(gain * 255)` clamped to
  `[0, 255]`. This matches `vec4_from_rgba`'s little-endian byte order in
  vendor/obs-studio/libobs/graphics/vec4.h (u[0] = the LOWEST byte = R).

The OBS-WebSocket I/O (screenshot sampling, filter creation/settings) reuses the SAME
`obs_phase2._conn`/`_rpc` helpers every other rig script uses -- never a second client.
"""
from __future__ import annotations

import argparse
import base64
import io
import json
import sys
import time

COLOUR_FILTER_KIND = "color_filter_v2"
DEFAULT_FILTER_NAME = "Colour Correction (#738)"

# Grey-world gains are clamped to this range -- a single-sample white-balance guess pushed
# further than this is more likely measuring biased scene content than a real hardware cast.
GAIN_MIN = 0.3
GAIN_MAX = 2.0

# The default (identity) OBS color_multiply setting -- R=G=B=255 (gain 1.0 each), alpha byte 0
# (unused by the shader's color_matrix diagonal -- see vec4_from_rgba's .w component, never read
# for color_multiply/color_add). Matches the live-observed OBS default exactly.
IDENTITY_COLOR_MULTIPLY = 0x00FFFFFF


def chroma_cast_bt601(mean_r: float, mean_g: float, mean_b: float) -> tuple[float, float, float]:
    """BT.601 RGB->Cb/Cr chroma-cast metric on a frame's MEAN channel values (a colour-neutral
    frame has Cb=Cr=0; a directional cast shows as a nonzero (Cb,Cr) vector). Returns
    (cb, cr, magnitude)."""
    cb = -0.168736 * mean_r - 0.331264 * mean_g + 0.5 * mean_b
    cr = 0.5 * mean_r - 0.418688 * mean_g - 0.081312 * mean_b
    mag = (cb * cb + cr * cr) ** 0.5
    return cb, cr, mag


def grey_world_gains(
    mean_r: float, mean_g: float, mean_b: float, damping: float = 0.6
) -> tuple[float, float, float]:
    """The damped per-channel gain that would bring (mean_r, mean_g, mean_b) toward a neutral
    grey, anchored on the DARKEST channel (`target = min(mean_r, mean_g, mean_b)`), NOT the
    mean of the three.

    This anchor choice is load-bearing, not cosmetic: OBS's `color_filter_v2` `color_multiply`
    setting is a plain byte 0..255 read as a [0.0, 1.0] linear multiplier (see
    `pack_color_multiply`'s doc) -- it can only DIM a channel, never boost one above its input
    level. Anchoring on the MEAN would routinely need a gain > 1.0 on whichever channel(s) sit
    below the mean; `pack_color_multiply` clamps that to byte 255 (gain 1.0, a silent no-op) --
    confirmed live, 2026-07-13: with a mean-anchored target, the "boosted" channel (G, needing
    gain up to ~1.9) never actually moved across 4 correction rounds, since every one of those
    gains clamped to a no-op. Anchoring on the min channel keeps every computed gain <= 1.0
    (that channel's own gain is exactly 1.0, a no-op; the others are proper attenuations),
    so every gain is representable and the filter can only ever DIM toward neutral -- the
    correction makes the image somewhat DARKER, never brighter; that is the real trade-off of a
    multiply-only (no `color_add` boost) instrument.

    `damping` in [0,1]: 1.0 applies the FULL correction, 0.0 is a no-op (gains all 1.0). Each
    gain is clamped to [GAIN_MIN, GAIN_MAX] -- a real hardware cast is a modest, bounded
    correction, never an extreme one; an extreme computed gain is a signal the input sample is
    unreliable (e.g. a near-black frame with almost no real signal), not a genuine correction to
    apply.

    Degenerate inputs (any channel mean <= 0, e.g. a fully black frame -- no signal to balance
    against) return (1.0, 1.0, 1.0) -- a no-op, never a division by zero or a wild gain.
    """
    if mean_r <= 0 or mean_g <= 0 or mean_b <= 0:
        return (1.0, 1.0, 1.0)
    target = min(mean_r, mean_g, mean_b)
    gains = []
    for channel in (mean_r, mean_g, mean_b):
        ideal = target / channel
        damped = 1.0 + damping * (ideal - 1.0)
        gains.append(max(GAIN_MIN, min(GAIN_MAX, damped)))
    return tuple(gains)  # type: ignore[return-value]


def compose_gains(
    base: tuple[float, float, float], correction: tuple[float, float, float]
) -> tuple[float, float, float]:
    """Elementwise-multiply two gain triples (each channel independently), clamped to
    [GAIN_MIN, GAIN_MAX] -- used to accumulate an iterative correction (round 2's correction
    factor, computed from round 1's ALREADY-corrected appearance, composes ONTO round 1's
    cumulative gain -- it does not replace it). This is needed because OBS's `color` setting is
    interpreted through an sRGB gamma decode before use as a linear multiplier
    (`vec4_from_rgba_srgb`, vendor/obs-studio/libobs/graphics/vec4.h), while a screenshot's pixel
    values are gamma-ENCODED (sRGB) -- so a single grey-world pass computed directly in
    screenshot-space under-corrects (confirmed live, 2026-07-13: a damping=1.0 "full" correction
    only reduced the measured cast by ~27%, not to ~0 as the naive linear-domain model predicts).
    Iterating -- measure the ACTUAL post-correction result, compute a fresh correction relative
    to it, and COMPOSE (not replace) -- converges empirically regardless of the exact gamma
    curve, the same way a real feedback-control loop does.
    """
    composed = tuple(base[i] * correction[i] for i in range(3))
    return tuple(max(GAIN_MIN, min(GAIN_MAX, g)) for g in composed)  # type: ignore[return-value]


def pack_color_multiply(gain_r: float, gain_g: float, gain_b: float) -> int:
    """Pack three per-channel gains into OBS's `color_multiply` int encoding (see module doc for
    the confirmed byte layout). A gain of 1.0 packs to byte 255 (0xFF), matching the identity
    default 0x00FFFFFF at (1.0, 1.0, 1.0)."""

    def _byte(gain: float) -> int:
        return max(0, min(255, round(gain * 255)))

    r, g, b = _byte(gain_r), _byte(gain_g), _byte(gain_b)
    return (0 << 24) | (b << 16) | (g << 8) | r


def unpack_color_multiply(value: int) -> tuple[float, float, float]:
    """Inverse of `pack_color_multiply` -- returns (gain_r, gain_g, gain_b)."""
    r = value & 0xFF
    g = (value >> 8) & 0xFF
    b = (value >> 16) & 0xFF
    return (r / 255.0, g / 255.0, b / 255.0)


def classify_persisted_correction(
    filter_present: bool, filter_enabled: bool | None, color_multiply: int | None
) -> str:
    """#738 drift-guard FACET -- the pure classification a persistence check reduces to (a
    scene-collection reset, a manual filter deletion, or a reinstall could silently drop this
    correction the same way #334's disabled-burn-filter and #522's saved_projectors leak already
    proved OBS state CAN quietly revert). Returns one of:

    - `"missing"` -- the filter is not attached at all (scene collection reset / never applied).
    - `"disabled"` -- present but disabled (never renders, #334's exact failure shape).
    - `"identity"` -- present + enabled, but `color_multiply` is still the neutral default
      (`IDENTITY_COLOR_MULTIPLY`) -- i.e. never actually calibrated, or reset back to it.
    - `"applied"` -- present, enabled, and carrying a genuine (non-identity) correction: the
      healthy state.

    Pure -- the caller (a drift-guard-style periodic check) supplies the THREE live-read facts;
    this function only classifies them, mirroring `obs_burn_filter.py`'s `compute_burn_on` shape.
    """
    if not filter_present:
        return "missing"
    if not filter_enabled:
        return "disabled"
    if color_multiply is None or color_multiply == IDENTITY_COLOR_MULTIPLY:
        return "identity"
    return "applied"


def check_correction_persisted(ws, source: str, filter_name: str = DEFAULT_FILTER_NAME) -> dict:
    """Live read-only check (the WS glue around `classify_persisted_correction`): has `source`'s
    colour-correction filter survived (present, enabled, non-identity)? Suitable for a periodic
    drift-guard-style assertion -- reuses the SAME `GetSourceFilterList` shape
    `obs_burn_filter.py`'s own `_has_filter`/`_filter_enabled` already use."""
    from obs_phase2 import _rpc

    data = _rpc(ws, "GetSourceFilterList", {"sourceName": source}, ignore_err=True)
    filters = (data or {}).get("filters", [])
    match = next((f for f in filters if f.get("filterName") == filter_name), None)
    present = match is not None
    enabled = bool(match.get("filterEnabled")) if match else None
    color_multiply = None
    if present:
        got = _rpc(ws, "GetSourceFilter", {"sourceName": source, "filterName": filter_name}, ignore_err=True)
        color_multiply = (got or {}).get("filterSettings", {}).get("color_multiply")
    status = classify_persisted_correction(present, enabled, color_multiply)
    return {
        "source": source,
        "status": status,
        "filter_present": present,
        "filter_enabled": enabled,
        "color_multiply": color_multiply,
    }


# ---------------------------------------------------------------------------
# OBS WebSocket I/O (real rig glue -- not unit-tested against a live OBS; the pure math above is)
# ---------------------------------------------------------------------------


def _screenshot_mean_rgb(ws, source: str, width: int = 320, height: int = 180):
    """One GetSourceScreenshot sample's mean (R, G, B) over the whole frame, or None if the
    screenshot could not be obtained (source off-program, RPC failure, etc -- #722's own
    fail-closed convention: None means 'could not check', never a fabricated reading)."""
    import numpy as np
    from PIL import Image

    from obs_phase2 import _rpc  # local import: keeps this module importable without a live OBS

    res = _rpc(
        ws,
        "GetSourceScreenshot",
        {"sourceName": source, "imageFormat": "png", "imageWidth": width, "imageHeight": height},
        ignore_err=True,
    )
    data = res.get("imageData") if res else None
    if not data:
        return None
    b64 = data.split(",", 1)[1] if data.startswith("data:") else data
    png_bytes = base64.b64decode(b64)
    img = Image.open(io.BytesIO(png_bytes)).convert("RGB")
    arr = np.asarray(img).astype(np.float64).reshape(-1, 3)
    return tuple(arr.mean(axis=0))


def sample_mean_rgb_over_time(ws, source: str, samples: int = 3, interval_s: float = 1.0):
    """Average `samples` screenshots taken `interval_s` apart -- a little temporal robustness
    against one biased frame (motion, a passing shadow), cheap compared to a proper multi-second
    AWB but strictly better than a single still. Returns None if EVERY sample failed."""
    readings = []
    for i in range(samples):
        r = _screenshot_mean_rgb(ws, source)
        if r is not None:
            readings.append(r)
        if i < samples - 1:
            time.sleep(interval_s)
    if not readings:
        return None
    n = len(readings)
    return tuple(sum(r[c] for r in readings) / n for c in range(3))


def ensure_colour_correction_filter(ws, source: str, filter_name: str = DEFAULT_FILTER_NAME):
    """Idempotent: create the color_filter_v2 filter on `source` if absent (mirrors
    obs_burn_filter.py's _ensure_filter pattern -- same connection helpers, same idempotency
    shape). Does NOT touch its settings -- callers apply settings separately."""
    from obs_phase2 import _rpc

    data = _rpc(ws, "GetSourceFilterList", {"sourceName": source}, ignore_err=True)
    present = any(f.get("filterName") == filter_name for f in (data or {}).get("filters", []))
    if not present:
        kinds = _rpc(ws, "GetSourceFilterKindList", ignore_err=True)
        if COLOUR_FILTER_KIND not in set((kinds or {}).get("sourceFilterKinds", [])):
            sys.exit(
                f"FAIL: filter kind '{COLOUR_FILTER_KIND}' is not registered on this OBS "
                "(too old a build?)."
            )
        _rpc(
            ws,
            "CreateSourceFilter",
            {"sourceName": source, "filterName": filter_name, "filterKind": COLOUR_FILTER_KIND},
        )


def calibrate_source(
    ws,
    source: str,
    filter_name: str = DEFAULT_FILTER_NAME,
    damping: float = 0.6,
    samples: int = 3,
    interval_s: float = 1.0,
    apply: bool = True,
):
    """Sample `source`'s current mean RGB, compute the grey-world color_multiply correction, and
    (if `apply`) set it on the filter. Returns a dict with the before-reading, computed gains,
    and the after-reading (re-sampled once applied) -- the evidence a decision is made from,
    never a bare "done"."""
    before = sample_mean_rgb_over_time(ws, source, samples, interval_s)
    if before is None:
        return {"source": source, "error": "no screenshot obtained -- source off-program?"}
    before_cb, before_cr, before_mag = chroma_cast_bt601(*before)
    gains = grey_world_gains(*before, damping=damping)
    result = {
        "source": source,
        "before_mean_rgb": before,
        "before_chroma": {"cb": before_cb, "cr": before_cr, "mag": before_mag},
        "gains": gains,
    }
    if apply:
        ensure_colour_correction_filter(ws, source, filter_name)
        from obs_phase2 import _rpc

        _rpc(
            ws,
            "SetSourceFilterSettings",
            {
                "sourceName": source,
                "filterName": filter_name,
                "filterSettings": {"color_multiply": pack_color_multiply(*gains)},
                "overlay": True,
            },
        )
        after = sample_mean_rgb_over_time(ws, source, samples, interval_s)
        if after is not None:
            after_cb, after_cr, after_mag = chroma_cast_bt601(*after)
            result["after_mean_rgb"] = after
            result["after_chroma"] = {"cb": after_cb, "cr": after_cr, "mag": after_mag}
    return result


def calibrate_source_iterative(
    ws,
    source: str,
    filter_name: str = DEFAULT_FILTER_NAME,
    damping: float = 0.6,
    samples: int = 3,
    interval_s: float = 1.0,
    rounds: int = 3,
):
    """Iteratively converge the grey-world correction (see `compose_gains`'s doc for WHY a single
    pass under-corrects -- OBS's `color` setting is sRGB-gamma-decoded before use as a linear
    multiplier, so a screenshot-domain grey-world computation doesn't invert cleanly in one
    shot). Each round measures the CURRENT rendered result, computes a fresh correction relative
    to it, and COMPOSES it onto the cumulative gain (never replaces). Returns a dict with the
    per-round trace (mean RGB + chroma cast after each round) -- the full evidence trail, not
    just the final number."""
    ensure_colour_correction_filter(ws, source, filter_name)
    from obs_phase2 import _rpc

    cumulative = (1.0, 1.0, 1.0)
    trace = []
    for round_i in range(rounds):
        mean = sample_mean_rgb_over_time(ws, source, samples, interval_s)
        if mean is None:
            trace.append({"round": round_i, "error": "no screenshot obtained"})
            break
        cb, cr, mag = chroma_cast_bt601(*mean)
        correction = grey_world_gains(*mean, damping=damping)
        cumulative = compose_gains(cumulative, correction)
        _rpc(
            ws,
            "SetSourceFilterSettings",
            {
                "sourceName": source,
                "filterName": filter_name,
                "filterSettings": {"color_multiply": pack_color_multiply(*cumulative)},
                "overlay": True,
            },
        )
        trace.append(
            {
                "round": round_i,
                "mean_rgb": mean,
                "chroma": {"cb": cb, "cr": cr, "mag": mag},
                "cumulative_gains": cumulative,
            }
        )
    return {"source": source, "cumulative_gains": cumulative, "trace": trace}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--source", action="append", required=True, dest="sources")
    ap.add_argument("--filter-name", default=DEFAULT_FILTER_NAME)
    ap.add_argument("--damping", type=float, default=0.6)
    ap.add_argument("--samples", type=int, default=3)
    ap.add_argument("--interval-s", type=float, default=1.0)
    ap.add_argument("--dry-run", action="store_true", help="measure + compute gains, never apply")
    ap.add_argument(
        "--iterative",
        type=int,
        default=0,
        metavar="ROUNDS",
        help="run ROUNDS of iterative composed correction instead of a single pass (see "
        "compose_gains's doc for why one pass under-corrects)",
    )
    ap.add_argument(
        "--check",
        action="store_true",
        help="#738 drift-guard facet: read-only -- report whether each source's correction is "
        "still present/enabled/non-identity, never measure or apply anything",
    )
    a = ap.parse_args(argv)

    from obs_phase2 import _conn

    ws = _conn(a.host, a.password)
    try:
        if a.check:
            results = [check_correction_persisted(ws, src, a.filter_name) for src in a.sources]
        elif a.iterative > 0:
            results = [
                calibrate_source_iterative(
                    ws, src, a.filter_name, a.damping, a.samples, a.interval_s, a.iterative
                )
                for src in a.sources
            ]
        else:
            results = [
                calibrate_source(
                    ws,
                    src,
                    a.filter_name,
                    a.damping,
                    a.samples,
                    a.interval_s,
                    apply=not a.dry_run,
                )
                for src in a.sources
            ]
    finally:
        ws.close()
    print(json.dumps(results, indent=2))
    if a.check:
        return 0 if all(r.get("status") == "applied" for r in results) else 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
