"""#1267 — the av-sync dock measured-offset trend must FLOW through the server's gather (not just the
pure parser), i.e. gather_bundle_state must expose the `av_offset_*` facets in the served bundle-state
dict, read from the SAME single bounded OBS-log read as the #1226/#1231 audio facets (no second scan).

Loaded by file path via importlib (bundle-state-server.py is hyphenated + __main__-guarded, so it
never starts the HTTP server), under a DISTINCT module name so it never collides with
test_bundle_state_server_log.py in one pytest process. Same "wiring flows through gather" proof shape
as that file's test_gather_bundle_state_exposes_audio_ts_lag_facet.
"""
import importlib.util
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

_SPEC = importlib.util.spec_from_file_location(
    "bundle_state_server_av_step_1267", _SCRIPTS / "bundle-state-server.py"
)
bss = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bss)  # __name__ != "__main__" -> main()/serve_forever() does NOT run


def _gather_with_log(monkeypatch, tmp_path, log_text):
    monkeypatch.setattr(bss, "gather_ndi_inputs", lambda host, password: {})
    log_dir = tmp_path / "logs"
    log_dir.mkdir()
    (log_dir / "obs.txt").write_text(log_text, encoding="utf-8")
    return bss.gather_bundle_state(
        "127.0.0.1", "", str(log_dir), str(tmp_path / "missing-ndi.dll"), [],
        genlock_build_sha_file=str(tmp_path / "missing-sha.txt"),
        startup_shortcut=str(tmp_path / "missing.lnk"),
        ahk_path=str(tmp_path / "missing.ahk"),
        obs_dll_path=str(tmp_path / "missing-obs.dll"),
    )


def _dock_lines():
    lines = []
    # baseline (10-40 min behind head) pin 926 offset 68; recent (<10 min) pin 926 offset 8; head at 18:10.
    for m in range(35, 44):
        lines.append(f"17:{m}:00.000: [obs-audio-video-sync-dock] av-sync-dock: LOCK-CORRECT SUGGESTED "
                     f"genlock_latency_ms_src 926 -> 858ms (measured offset=68.0ms) [monitor-only -- x]")
    for i in range(9):
        lines.append(f"18:0{i}:00.000: [obs-audio-video-sync-dock] av-sync-dock: LOCK-CORRECT SUGGESTED "
                     f"genlock_latency_ms_src 926 -> 918ms (measured offset=8.0ms) [monitor-only -- x]")
    lines.append("18:10:00.000: [obs] render tick — head")
    return "\n".join(lines) + "\n"


def test_gather_exposes_av_offset_facets(monkeypatch, tmp_path):
    state = _gather_with_log(monkeypatch, tmp_path, _dock_lines())
    assert state["av_offset_recent_med_ms"] == "8.0"
    assert state["av_offset_base_med_ms"] == "68.0"
    assert state["av_offset_pin"] == "926"
    assert state["av_offset_pin_stable"] == "1"
    assert int(state["av_offset_age_s"]) < 300
    assert int(state["av_offset_n_recent"]) >= 6
    assert int(state["av_offset_n_base"]) >= 6


def test_gather_omits_av_offset_facets_when_no_dock_lines(monkeypatch, tmp_path):
    # a box whose OBS log has no dock line (e.g. strih, no 'mbc' source) omits the facets entirely
    # (absent == UNKNOWN downstream, never a fabricated reading).
    state = _gather_with_log(monkeypatch, tmp_path, "18:00:00.000: [obs] render tick\n")
    assert "av_offset_recent_med_ms" not in state
    assert "av_offset_base_med_ms" not in state
    assert "av_offset_pin" not in state
