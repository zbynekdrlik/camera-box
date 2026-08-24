"""#756 Member 3 -- unit tests for scripts/latency_pins_snapshot.py, the live per-source genlock
latency pins snapshot + recommended-pins gatherer feeding scripts/e2e_discord_report.py's
_section_latency_pins.

Covers, with NO live OBS/network:
  a. delivery_p50_table() -- pure extraction of {camN: p50_ms} from a verdict dict.
  b. read_pin() -- honest None on a missing key / missing source / a failed RPC, never a
     silently-defaulted floor value.
  c. active_camera_numbers() (#893) -- derives the camera numbers to sweep from
     CAMERA_ACTIVE_SET, never a literal N=1..7 range.
  d. snapshot_box_pins() -- reads main+MV for every camera in the ACTIVE set (#893 -- was a
     literal cam1..7 sweep) via the given name templates; returns {} (never a half-filled
     table) on a connect failure.
  e. load_av_sync_last() -- reads the source-of-truth JSON; {} when absent/malformed.
"""
import json
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import latency_pins_snapshot as lps  # noqa: E402


# ---------------------------------------------------------------------------
# delivery_p50_table -- pure
# ---------------------------------------------------------------------------

class TestDeliveryP50Table:
    def test_extracts_p50_per_camera(self):
        verdict = {
            "all_cambox_delivery_latency": {
                "cam1": {"p50_ms": 71.2, "mean_ms": 71.2},
                "cam2": {"p50_ms": 68.0},
                "cross_camera_spread_ms": 3.2,  # not a camera -- must be ignored
                "spread_gate_pass": True,
            }
        }
        assert lps.delivery_p50_table(verdict) == {"cam1": 71.2, "cam2": 68.0}

    def test_missing_block_returns_empty(self):
        assert lps.delivery_p50_table({}) == {}

    def test_camera_without_p50_field_is_skipped(self):
        verdict = {"all_cambox_delivery_latency": {"cam1": {"mean_ms": 71.2}}}
        assert lps.delivery_p50_table(verdict) == {}

    def test_non_dict_block_returns_empty(self):
        assert lps.delivery_p50_table({"all_cambox_delivery_latency": "not a dict"}) == {}


# ---------------------------------------------------------------------------
# read_pin -- honest None on any failure/absence, never a fabricated default
# ---------------------------------------------------------------------------

class FakeWS:
    def __init__(self):
        self.closed = False

    def close(self):
        self.closed = True


class TestReadPin:
    def test_reads_the_configured_value(self, monkeypatch):
        def fake_rpc(ws, rtype, rdata=None, ignore_err=False):
            assert rtype == "GetInputSettings"
            assert rdata == {"inputName": "NDI cam1"}
            return {"inputSettings": {"genlock_latency_ms_src": 14}}

        monkeypatch.setattr(lps, "_rpc", fake_rpc)
        assert lps.read_pin(FakeWS(), "NDI cam1") == 14

    def test_missing_key_is_honest_none_not_a_fabricated_floor(self, monkeypatch):
        monkeypatch.setattr(lps, "_rpc", lambda *a, **k: {"inputSettings": {}})
        assert lps.read_pin(FakeWS(), "NDI cam9-does-not-exist") is None

    def test_rpc_exception_is_honest_none(self, monkeypatch):
        def raising_rpc(*a, **k):
            raise RuntimeError("GetInputSettings failed: source not found")

        monkeypatch.setattr(lps, "_rpc", raising_rpc)
        assert lps.read_pin(FakeWS(), "NDI cam1") is None

    def test_non_numeric_value_is_honest_none(self, monkeypatch):
        monkeypatch.setattr(
            lps, "_rpc", lambda *a, **k: {"inputSettings": {"genlock_latency_ms_src": "not-a-number"}}
        )
        assert lps.read_pin(FakeWS(), "NDI cam1") is None


# ---------------------------------------------------------------------------
# active_camera_numbers -- #893: derive from CAMERA_ACTIVE_SET, never a literal N=1..7 range
# ---------------------------------------------------------------------------

class TestActiveCameraNumbers:
    def test_default_is_cam3(self, monkeypatch):
        # issue 1170 (2026-08-24): cam2's camera-under-test role retired (grabber cure-decay);
        # default mirrors camera-set.sh's CAMERA_ACTIVE_SET = "cam3" (cam2 stays the painter only).
        monkeypatch.delenv("CAMERA_ACTIVE_SET", raising=False)
        assert lps.active_camera_numbers() == (3,)

    def test_env_override_narrows_the_set(self, monkeypatch):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam3")
        assert lps.active_camera_numbers() == (1, 3)

    def test_env_override_widens_to_a_retired_camera(self, monkeypatch):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam2 cam3 cam4 cam5")
        assert lps.active_camera_numbers() == (1, 2, 3, 4, 5)

    def test_comma_separated_override_is_also_accepted(self, monkeypatch):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1,cam2")
        assert lps.active_camera_numbers() == (1, 2)


# ---------------------------------------------------------------------------
# snapshot_box_pins -- main+MV for every camera in the ACTIVE set (#893), honest {} on connect
# failure
# ---------------------------------------------------------------------------

class TestSnapshotBoxPins:
    def test_reads_main_and_mv_for_each_camera_in_the_default_active_set(self, monkeypatch):
        # issue 1170 (2026-08-24): cam2's camera-under-test role retired -> default active set is
        # cam3 alone, never the old literal cam1..7 sweep. A retired camera
        # (cam1/cam2/cam4/cam5/cam6/cam7) must not even be attempted.
        monkeypatch.delenv("CAMERA_ACTIVE_SET", raising=False)
        monkeypatch.setattr(lps, "_conn", lambda host, password: FakeWS())

        def fake_read_pin(ws, name):
            # "NDI cam3" -> main=3, "MV NDI cam3" -> mv=103 (deterministic per-name stub)
            n = int("".join(ch for ch in name if ch.isdigit()))
            return n if "MV" not in name else 100 + n

        monkeypatch.setattr(lps, "read_pin", fake_read_pin)
        result = lps.snapshot_box_pins("10.77.9.202", "", "NDI cam{n}", "MV NDI cam{n}")
        assert len(result) == 1
        assert set(result.keys()) == {"cam3"}
        assert result["cam3"] == {"main_ms": 3, "mv_ms": 103}

    def test_camera_active_set_env_override_narrows_the_sweep(self, monkeypatch):
        # #893: CAMERA_ACTIVE_SET is the ONE source of truth for which cameras get swept --
        # never a second hardcoded list. Overriding it to a subset must narrow the read, and a
        # camera OUTSIDE the override (even one inside the module's own default) must not be
        # read at all.
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam3")
        monkeypatch.setattr(lps, "_conn", lambda host, password: FakeWS())

        def fake_read_pin(ws, name):
            n = int("".join(ch for ch in name if ch.isdigit()))
            return n if "MV" not in name else 100 + n

        monkeypatch.setattr(lps, "read_pin", fake_read_pin)
        result = lps.snapshot_box_pins("10.77.9.202", "", "NDI cam{n}", "MV NDI cam{n}")
        assert set(result.keys()) == {"cam1", "cam3"}

    def test_camera_active_set_env_override_widens_the_sweep_to_a_retired_camera(self, monkeypatch):
        # #827-style reversibility: re-adding a retired camera to CAMERA_ACTIVE_SET must make it
        # swept again with zero code changes -- mirrors camera-set.sh's own re-enable procedure.
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam2 cam3 cam4 cam5")
        monkeypatch.setattr(lps, "_conn", lambda host, password: FakeWS())

        def fake_read_pin(ws, name):
            n = int("".join(ch for ch in name if ch.isdigit()))
            return n if "MV" not in name else 100 + n

        monkeypatch.setattr(lps, "read_pin", fake_read_pin)
        result = lps.snapshot_box_pins("10.77.9.202", "", "NDI cam{n}", "MV NDI cam{n}")
        assert "cam5" in result
        assert "cam6" not in result

    def test_connect_failure_returns_empty_never_a_half_filled_table(self, monkeypatch):
        def raising_conn(host, password):
            raise ConnectionRefusedError("no route to host")

        monkeypatch.setattr(lps, "_conn", raising_conn)
        assert lps.snapshot_box_pins("10.77.9.202", "", "NDI cam{n}", "MV NDI cam{n}") == {}

    def test_closes_the_websocket_even_if_a_read_raises(self, monkeypatch):
        ws = FakeWS()
        monkeypatch.setattr(lps, "_conn", lambda host, password: ws)

        calls = {"n": 0}

        def failing_read_pin(w, name):
            calls["n"] += 1
            if calls["n"] == 2:
                raise RuntimeError("boom -- deliberate mid-loop failure to prove the finally-close runs")
            return 3

        monkeypatch.setattr(lps, "read_pin", failing_read_pin)
        with pytest.raises(RuntimeError, match="boom"):
            lps.snapshot_box_pins("10.77.9.202", "", "NDI cam{n}", "MV NDI cam{n}")
        assert ws.closed is True


# ---------------------------------------------------------------------------
# load_av_sync_last -- the source-of-truth applied stream hold
# ---------------------------------------------------------------------------

class TestLoadAvSyncLast:
    def test_reads_the_real_file(self, tmp_path, monkeypatch):
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        monkeypatch.setattr(lps.Path, "home", classmethod(lambda cls: tmp_path))
        d = tmp_path / ".camera-box"
        d.mkdir()
        payload = {"applied_latency_ms": 952, "source": "NDI 2ME PGM"}
        (d / "av-sync-last.json").write_text(json.dumps(payload), encoding="utf-8")
        assert lps.load_av_sync_last() == payload

    def test_missing_file_returns_empty(self, tmp_path, monkeypatch):
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        monkeypatch.setattr(lps.Path, "home", classmethod(lambda cls: tmp_path))
        assert lps.load_av_sync_last() == {}

    def test_malformed_json_returns_empty_not_a_crash(self, tmp_path, monkeypatch):
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        monkeypatch.setattr(lps.Path, "home", classmethod(lambda cls: tmp_path))
        d = tmp_path / ".camera-box"
        d.mkdir()
        (d / "av-sync-last.json").write_text("{not valid json", encoding="utf-8")
        assert lps.load_av_sync_last() == {}

    def test_programdata_env_wins_when_set(self, tmp_path, monkeypatch):
        monkeypatch.setenv("PROGRAMDATA", str(tmp_path))
        assert lps.av_sync_last_path() == tmp_path / "camera-box" / "av-sync-last.json"
