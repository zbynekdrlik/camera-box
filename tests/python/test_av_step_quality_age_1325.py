"""#1325 — the dock quality-line FRESHNESS gate on the av-step band/step arms.

Owner report 16.9.2026: the av-step BAND arm paged the owner 3× with `av_offset_recent_mad_ms=None`.
Root cause: the QPSK marker cadence dropped to 0.5 s, the dock's decoder stopped emitting its
`UPDATED/LOCKED … matched= mad=` QUALITY lines, yet its SUGGESTED offset SERIES kept producing
(stale) offsets — so `band_quality_ok(None)` returned None and #1319's "absent quality -> proceed,
never swallow a real drift" paged on an untrustworthy reading.

This lane adds `bundle_state_gather.av_offset_quality_age_from_log` (the in-log age of the freshest
dock quality line) and gates the ABSENT-quality "proceed" on it. `band_quality_ok` returns None only
when NO quality line exists in the recent window, so the honest gate is: absent quality + a PRESENT
age (a quality line existed earlier == the decoder went stale) -> LOW_QUALITY (no page); absent
quality + an ABSENT age (no quality line anywhere == older box / dock never locked) -> proceed, so
#1319's real-drift protection is preserved (a drift on a DECODING dock has quality present, judged).
"""
import importlib.util
import json
import pathlib

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"


def _load(name):
    spec = importlib.util.spec_from_file_location(name, _SCRIPTS / f"{name}.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


bsg = _load("bundle_state_gather")
asd = _load("av_step_decision")

_PFX = "[obs-audio-video-sync-dock] av-sync-dock:"


def _quality_line(h, m, s, matched=36, mad=9.0):
    return (f"{h:02d}:{m:02d}:{s:02d}.000: {_PFX} UPDATED offset=12ms source=cluster "
            f"matched={matched} mad={mad}ms")


def _plain(h, m, s, msg="diag locked=yes"):
    return f"{h:02d}:{m:02d}:{s:02d}.000: {_PFX} {msg}"


# ---- the gather parser: av_offset_quality_age_from_log ---------------------------------------

def test_quality_age_absent_when_no_quality_line():
    # A log with only heartbeat lines, never a quality line -> "" (absent -> UNKNOWN downstream).
    log = "\n".join(_plain(20, m, 0) for m in range(5))
    assert bsg.av_offset_quality_age_from_log(log) == ""


def test_quality_age_zero_when_fresh():
    # The freshest quality line IS the log head -> age 0.
    log = "\n".join([_plain(20, 0, 0), _quality_line(20, 1, 0)])
    assert bsg.av_offset_quality_age_from_log(log) == "0"


def test_quality_age_large_when_dock_stopped_updating():
    # A quality line at 20:00, then only heartbeats through 20:10 (the decoder stopped) -> ~600 s age.
    lines = [_quality_line(20, 0, 0)] + [_plain(20, m, 0) for m in range(1, 11)]
    age = bsg.av_offset_quality_age_from_log("\n".join(lines))
    assert age != ""
    assert int(age) >= 590   # ~10 min behind the log head


# ---- classify_av_band: the absent-quality freshness gate ------------------------------------

def _kw(**over):
    base = dict(recent_med=80.0, pin_stable="1", n_recent=10, dock_live_age_s=5,
                box_reachable=1, band_reference_ms=0.0, recent_mad_ms=None, recent_matched_min=None)
    base.update(over)
    return base


def test_band_absent_quality_stale_age_is_low_quality_no_page():
    # #1325 fix: quality absent (mad/matched None) AND the dock stopped updating (age > stale) ->
    # LOW_QUALITY, NOT the OUT_OF_BAND page the owner got 3× on 16.9.
    v = asd.classify_av_band(**_kw(quality_age_s=999))
    assert v == "LOW_QUALITY"


def test_band_absent_quality_present_age_is_low_quality():
    # #1325: quality absent (mad/matched None) but a dock quality line existed earlier (age present ==
    # the dock decoded before but stopped) -> LOW_QUALITY, NOT the OUT_OF_BAND page. `band_quality_ok`
    # returns None only when there is no quality line in the recent window, so a present age here
    # always means the decoder went stale — a small age value is production-impossible in this branch.
    assert asd.classify_av_band(**_kw(quality_age_s=10)) == "LOW_QUALITY"
    assert asd.classify_av_band(**_kw(quality_age_s=999)) == "LOW_QUALITY"


def test_band_absent_quality_absent_age_still_pages():
    # The ONLY absent-quality "proceed" case: an OLDER box / dock that NEVER emitted a quality line
    # (age None) keeps #1319's absent->proceed, so a genuine sustained offset is never swallowed.
    v = asd.classify_av_band(**_kw(quality_age_s=None))
    assert v == "OUT_OF_BAND"


def test_band_present_good_quality_unaffected_by_age():
    # A present, trustworthy reading is judged on the band regardless of the quality age.
    v = asd.classify_av_band(**_kw(recent_mad_ms=8.0, recent_matched_min=40, quality_age_s=999))
    assert v == "OUT_OF_BAND"


def test_band_present_bad_quality_is_low_quality():
    # A present but too-noisy reading is LOW_QUALITY (the #1319 P2 gate), age irrelevant.
    v = asd.classify_av_band(**_kw(recent_mad_ms=25.0, recent_matched_min=12, quality_age_s=10))
    assert v == "LOW_QUALITY"


# ---- classify_av_step: same gate --------------------------------------------------------------

def _skw(**over):
    base = dict(recent_med=500.0, base_med=400.0, pin_stable="1", age_s=5, n_recent=10, n_base=10,
                box_reachable=1, recent_mad_ms=None, recent_matched_min=None)
    base.update(over)
    return base


def test_step_absent_quality_present_age_is_low_quality():
    # Any present age in the absent-quality branch means the dock stopped decoding -> LOW_QUALITY.
    assert asd.classify_av_step(**_skw(quality_age_s=999)) == "LOW_QUALITY"
    assert asd.classify_av_step(**_skw(quality_age_s=10)) == "LOW_QUALITY"


def test_step_absent_quality_absent_age_still_steps():
    # Age None (no quality line anywhere = older box) keeps #1319's absent->proceed.
    assert asd.classify_av_step(**_skw(quality_age_s=None)) == "STEP"


# ---- analyze / analyze_band read the new facet from the JSON body ---------------------------

def test_analyze_band_reads_quality_age_facet_and_suppresses_page():
    body = json.dumps({
        "av_offset_recent_med_ms": 80.0, "av_offset_pin_stable": "1", "av_offset_n_recent": 10,
        "av_offset_dock_live_age_s": 5,
        # quality facet ABSENT (the dock stopped decoding), and its age is STALE:
        "av_offset_quality_age_s": 999,
    })
    res = asd.analyze_band(body, 1)
    assert res["verdict"] == "LOW_QUALITY"


def test_analyze_step_reads_quality_age_facet_and_suppresses_page():
    body = json.dumps({
        "av_offset_recent_med_ms": 500.0, "av_offset_base_med_ms": 400.0,
        "av_offset_pin_stable": "1", "av_offset_age_s": 5,
        "av_offset_n_recent": 10, "av_offset_n_base": 10,
        "av_offset_quality_age_s": 999,
    })
    res = asd.analyze(body, 1)
    assert res["verdict"] == "LOW_QUALITY"
