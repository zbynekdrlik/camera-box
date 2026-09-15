"""#1303 -- tests for the LOCK-indicator AUDIO term (audible-but-expected-silent).

The in-OBS genlock LOCK indicator (#1298) + its fleet facet (#1299) judged VIDEO only; #1303 part 3b
added the box-agnostic audio pairing-offset DEGRADE, and THIS lane adds the certified-table
`audio_unexpected` term: a genlock source that is AUDIBLE when the per-box audio table expects it
silent (a camera on any box; the double-audio hazard the owner ruled on 2026-09-15). This file owns
the python half: the `decide()` mirror's two audio axes + `analyze()`'s reason enrichment
(`audio_unexpected:<name>`) + the gather parser's v4 `audio_unexpected_inputs` reshape. Pure, no
I/O -- Tier-0 (the #1199 python-mirror precedent), so it RED->GREENs locally under #557.
"""

import pathlib
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import genlock_lock_decision as d
import bundle_state_gather as g


def _healthy_args():
    # clock locked, every input locked, output stamping -> LOCKED before any audio axis.
    return dict(n_inputs=7, n_locked=7, recent_event=False, qpc_drift_beyond_bound=False,
                clock_present=True, clock_locked=True, clock_ntp_failed=False,
                output_present=True, output_stamping=True)


# ---- decide() mirror: the two audio axes -----------------------------------------------------

def test_audio_unexpected_degrades():
    assert d.decide(**_healthy_args(), audio_unexpected=True) == (d.ST_DEGRADED, d.R_AUDIO_UNEXPECTED)


def test_audio_unpaired_degrades():
    assert d.decide(**_healthy_args(), audio_unpaired=True) == (d.ST_DEGRADED, d.R_AUDIO_PAIRING)


def test_audio_unpaired_beats_audio_unexpected():
    # audio_unexpected is the lowest-precedence DEGRADED reason -- audio_pairing wins over it.
    assert d.decide(**_healthy_args(), audio_unpaired=True, audio_unexpected=True) \
        == (d.ST_DEGRADED, d.R_AUDIO_PAIRING)


def test_qpc_beats_both_audio_axes():
    # every video DEGRADED reason (here qpc) outranks the audio axes.
    a = _healthy_args()
    a["qpc_drift_beyond_bound"] = True
    assert d.decide(**a, audio_unexpected=True) == (d.ST_DEGRADED, d.R_QPC_DRIFT)


def test_audio_axes_never_rescue_unlocked():
    a = _healthy_args()
    a["clock_locked"] = False
    assert d.decide(**a, audio_unexpected=True, audio_unpaired=True) == (d.ST_UNLOCKED, d.R_CLOCK)


def test_audio_axes_default_false_reproduce_pre_1303():
    # no audio args -> the pre-#1303 healthy verdict is unchanged.
    assert d.decide(**_healthy_args()) == (d.ST_LOCKED, d.R_NONE)


# ---- analyze(): reason enrichment ------------------------------------------------------------

def _facet(reason, audio_unexpected_inputs=None):
    f = {"state": d.ST_DEGRADED, "reason": reason, "n_inputs": 7, "n_locked": 7, "n_absent": 0}
    if audio_unexpected_inputs is not None:
        f["audio_unexpected_inputs"] = audio_unexpected_inputs
    import json
    return json.dumps({"genlock_lock": f})


def test_analyze_enriches_audio_unexpected_with_offender():
    res = d.analyze(_facet("audio_unexpected", [{"name": "CAM3 (usb)"}]), 1)
    assert res["verdict"] == "DEGRADED"
    assert res["reason"] == "audio_unexpected:CAM3 (usb)"


def test_analyze_leaves_bare_audio_unexpected_when_no_offender():
    # a v1/v2/v3 line (no list) or an empty list -> the bare token, never a trailing colon.
    assert d.analyze(_facet("audio_unexpected", None), 1)["reason"] == "audio_unexpected"
    assert d.analyze(_facet("audio_unexpected", []), 1)["reason"] == "audio_unexpected"


def test_analyze_ignores_malformed_audio_offender():
    assert d.analyze(_facet("audio_unexpected", [{"noname": 1}]), 1)["reason"] == "audio_unexpected"
    assert d.analyze(_facet("audio_unexpected", ["notadict"]), 1)["reason"] == "audio_unexpected"


def test_analyze_does_not_touch_other_reasons_with_stray_audio_list():
    # a stray audio_unexpected_inputs on a non-audio reason never corrupts that reason.
    assert d.analyze(_facet("qpc_drift", [{"name": "cg"}]), 1)["reason"] == "qpc_drift"


# ---- gather: the v4 audio_unexpected_inputs reshape ------------------------------------------

def _line(json_payload):
    return "2026-09-15 10:00:00.000: genlock-lock-json: %s (#1299)" % json_payload


def test_gather_parses_audio_unexpected_inputs():
    line = _line('{"v":4,"state":"DEGRADED","reason":"audio_unexpected","n_inputs":7,"n_locked":7,'
                 '"audio_unexpected_inputs":[{"name":"CAM1 (usb)"}]}')
    facet = g.genlock_lock_facet_from_log(line)
    assert facet["audio_unexpected_inputs"] == [{"name": "CAM1 (usb)"}]


def test_gather_omits_audio_unexpected_inputs_when_empty():
    line = _line('{"v":4,"state":"LOCKED","reason":"none","audio_unexpected_inputs":[]}')
    facet = g.genlock_lock_facet_from_log(line)
    assert "audio_unexpected_inputs" not in facet


def test_gather_older_v3_line_reads_cleanly():
    # a pre-#1303 v3 line (no audio_unexpected_inputs key) parses with the key omitted.
    line = _line('{"v":3,"state":"LOCKED","reason":"none","recent_event_inputs":[]}')
    facet = g.genlock_lock_facet_from_log(line)
    assert "audio_unexpected_inputs" not in facet
    assert facet["state"] == "LOCKED"


def test_gather_skips_malformed_audio_offender_rows():
    line = _line('{"v":4,"state":"DEGRADED","reason":"audio_unexpected",'
                 '"audio_unexpected_inputs":[{"name":""},"x",{"name":"cg"}]}')
    facet = g.genlock_lock_facet_from_log(line)
    assert facet["audio_unexpected_inputs"] == [{"name": "cg"}]
