"""Issue 1302 — the genlock_lock facet passes each input's audio PLACEMENT through.

While the genlock audio pairing still withholds a freshly attached input's audio, the box-level
facet already read LOCKED/none: the per-input hold mode, withheld-packet count and placement error
existed only in the 5 s `genlock-fifo audit` log line. The LOCK widget's v8 `genlock-lock-json:`
heartbeat now carries them per input (`audio_hold` / `audio_withheld` / `audio_place_err_ms`, the
last null until a placement error is measured), and `genlock_lock_facet_from_log` passes them into
`genlock_lock.inputs["<name>"]` — OMITTED when the line does not carry them, so a v7 line (an older
build, or a pre-v4 libobs) parses exactly as before. A consumer such as the SongPlayer A/V gate can
then wait until its input's `audio_hold` is no longer `pending`.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402

_V7_INPUT_KEYS = {"locked", "connected", "idle", "latency_ms", "underruns", "relocks", "late_holds", "depth"}
_AUDIO_KEYS = {"audio_hold", "audio_withheld", "audio_place_err_ms"}


def _line(payload: str) -> str:
    return f"12:00:00.000: genlock-lock-json: {payload} (#1299)\n"


# The cg OBS right after the SongPlayer probe attached: the probe's audio is still withheld (no
# placement error yet), the program feed plays on its timecode hold, slightly early.
V8_LINE = _line(
    '{"v":8,"state":"LOCKED","reason":"none","n_inputs":2,"n_locked":2,"n_absent":0,"n_idle":0,'
    '"latency_ms":3,"clock":"locked","output":"stamping","recent_event":false,'
    '"recent_event_inputs":[],"audio_unexpected_inputs":[],"qpc_drift_ms":0,"inputs":['
    '{"name":"sp-probe","locked":true,"connected":true,"idle":false,"latency_ms":3,"underruns":0,'
    '"relocks":40,"late_holds":0,"depth":2,"audio_hold":"pending","audio_withheld":118,'
    '"audio_place_err_ms":null},'
    '{"name":"NDI 2ME PGM","locked":true,"connected":true,"idle":false,"latency_ms":3,"underruns":0,'
    '"relocks":3,"late_holds":0,"depth":2,"audio_hold":"timecode","audio_withheld":0,'
    '"audio_place_err_ms":-3}'
    '],"qpc_drift_ppm":0.000,"qpc_expected_ppm":0.000,"qpc_step":false,'
    '"media_clock":{"state":"ok","drift_us":0,"window_s":600,"ready":true,"discipline":"n/a"}}'
)

# The same box on a build before issue 1302 (schema v7): no audio keys at all.
V7_LINE = _line(
    '{"v":7,"state":"LOCKED","reason":"none","n_inputs":1,"n_locked":1,"n_absent":0,"n_idle":0,'
    '"latency_ms":3,"clock":"locked","output":"stamping","recent_event":false,'
    '"recent_event_inputs":[],"audio_unexpected_inputs":[],"qpc_drift_ms":0,"inputs":['
    '{"name":"sp-probe","locked":true,"connected":true,"idle":false,"latency_ms":3,"underruns":0,'
    '"relocks":40,"late_holds":0,"depth":2}'
    '],"qpc_drift_ppm":0.000,"qpc_expected_ppm":0.000,"qpc_step":false,'
    '"media_clock":{"state":"ok","drift_us":0,"window_s":600,"ready":true,"discipline":"n/a"}}'
)


def test_v8_line_passes_each_inputs_audio_placement_through():
    f = bsg.genlock_lock_facet_from_log(V8_LINE)
    assert f is not None and f["state"] == "LOCKED"
    probe = f["inputs"]["sp-probe"]
    assert probe["audio_hold"] == "pending"
    assert probe["audio_withheld"] == 118
    # null until a placement error has been measured: the key is there, the value is None
    assert "audio_place_err_ms" in probe and probe["audio_place_err_ms"] is None
    pgm = f["inputs"]["NDI 2ME PGM"]
    assert pgm["audio_hold"] == "timecode"
    assert pgm["audio_withheld"] == 0
    assert pgm["audio_place_err_ms"] == -3
    # the v7 per-input keys are untouched
    assert probe["relocks"] == 40 and probe["connected"] is True and probe["idle"] is False


def test_v7_line_parses_exactly_as_before():
    f = bsg.genlock_lock_facet_from_log(V7_LINE)
    assert f is not None and f["state"] == "LOCKED"
    probe = f["inputs"]["sp-probe"]
    assert set(probe) == _V7_INPUT_KEYS
    assert not _AUDIO_KEYS & set(probe)


def test_a_row_passes_only_the_audio_keys_it_carries():
    # A row that carries only the hold token (never emitted by the widget, which writes all three
    # together) still never fabricates the other two.
    line = _line(
        '{"v":8,"state":"LOCKED","reason":"none","n_inputs":1,"n_locked":1,"latency_ms":3,'
        '"clock":"locked","output":"absent","recent_event":false,"qpc_drift_ms":0,"inputs":['
        '{"name":"cg","locked":true,"latency_ms":3,"underruns":0,"relocks":0,"late_holds":0,'
        '"depth":2,"audio_hold":"latency"}]}'
    )
    cg = bsg.genlock_lock_facet_from_log(line)["inputs"]["cg"]
    assert cg["audio_hold"] == "latency"
    assert "audio_withheld" not in cg and "audio_place_err_ms" not in cg


def test_a_consumer_can_wait_for_placed_audio():
    # The question the SongPlayer gate asks: is the probe's audio placed yet? On the v8 line it is
    # not (pending), the program feed's is. On a v7 line the answer is unknown (no key), never "yes".
    def placed(facet, name):
        hold = facet["inputs"][name].get("audio_hold")
        return None if hold is None else hold in ("latency", "timecode")

    v8 = bsg.genlock_lock_facet_from_log(V8_LINE)
    assert placed(v8, "sp-probe") is False
    assert placed(v8, "NDI 2ME PGM") is True
    assert placed(bsg.genlock_lock_facet_from_log(V7_LINE), "sp-probe") is None
