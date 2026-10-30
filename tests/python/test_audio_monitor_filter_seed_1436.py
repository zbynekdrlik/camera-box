"""issue 1436 -- the launch seed adds the Audio Monitor filter to the stream box's program input.

The filter plays the program input to the operator's locked headphone sink (`stream-operator-headphones`), as the
Windows stream box's Audio Monitor plugin did. It is added only when the input has no `audio_monitor` filter yet:
an operator's own filter (another device, another volume) is never rewritten.
"""
import importlib.util
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"


def _scenes():
    spec = importlib.util.spec_from_file_location("strih_scenes_1436_filter", SCRIPTS / "strih_scenes.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


class FakeObs:
    def __init__(self, filters=None, plugin_loaded=True):
        self.filters = list(filters or [])
        self.plugin_loaded = plugin_loaded
        self.calls = []

    def req(self, req_type, data=None, ignore_err=False):
        self.calls.append((req_type, data))
        if req_type == "GetSourceFilterList":
            return {"filters": [dict(f) for f in self.filters]}
        if req_type == "CreateSourceFilter":
            if self.plugin_loaded:
                self.filters.append({"filterName": data["filterName"], "filterKind": data["filterKind"],
                                     "filterSettings": data["filterSettings"]})
            return {}
        if req_type == "GetSourceFilter":
            for f in self.filters:
                if f["filterName"] == data["filterName"]:
                    return dict(f)
            return {}
        raise AssertionError(req_type)


def test_the_filter_is_added_to_the_headphone_sink():
    s = _scenes()
    obs = FakeObs()
    st = s.seed_audio_monitor_filter(obs, "mbc", "stream-operator-headphones")
    assert st.startswith("added"), st
    (f,) = obs.filters
    assert f["filterKind"] == "audio_monitor"
    assert f["filterSettings"]["device"] == "stream-operator-headphones"


def test_an_operator_filter_is_kept_untouched():
    s = _scenes()
    own = {"filterName": "moj monitor", "filterKind": "audio_monitor", "filterSettings": {"device": "other"}}
    obs = FakeObs([own])
    st = s.seed_audio_monitor_filter(obs, "mbc", "stream-operator-headphones")
    assert st.startswith("kept") and "moj monitor" in st
    assert not [c for c in obs.calls if c[0] == "CreateSourceFilter"]


def test_a_missing_plugin_is_a_named_failure_never_a_pass():
    s = _scenes()
    st = s.seed_audio_monitor_filter(FakeObs(plugin_loaded=False), "mbc", "stream-operator-headphones")
    assert st.startswith("create FAILED") and "plugin" in st


def test_the_manifest_reader():
    s = _scenes()
    assert s.parse_program_audio_monitor_filter('{"program_audio_monitor_filter": "stream-operator-headphones"}') \
        == "stream-operator-headphones"
    assert s.parse_program_audio_monitor_filter('{"mode": "update-only"}') is None
    assert s.parse_program_audio_monitor_filter("not json") is None


def test_the_stream_manifest_names_the_sink_and_the_strih_one_does_not():
    facts = SCRIPTS / "lib" / "strih-box-facts.sh"
    prov = SCRIPTS / "lib" / "strih-provision.sh"
    out = {}
    for box, fn in (("stream-snv", "obs_box_stream_seed_manifest_json"), ("strih-snv", "strih_lx_seed_manifest_json")):
        r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; strih_box_load "$2"; . "$3"; "$4"',
                            "h", str(facts), box, str(prov), fn], capture_output=True, text=True,
                           env=dict(os.environ), timeout=60)
        assert r.returncode == 0, r.stderr
        out[box] = r.stdout
    s = _scenes()
    assert s.parse_program_audio_monitor_filter(out["stream-snv"]) == "stream-operator-headphones"
    assert s.parse_program_audio_monitor_filter(out["strih-snv"]) is None
