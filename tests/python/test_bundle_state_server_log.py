"""#829 — regression test for scripts/bundle-state-server.py's log() surviving a DEAD stdout pipe.

Live incident (stream box, 2026-08-15): a hidden Scheduled-Task context handed the process a dead
stdout pipe, so `print(..., flush=True)` raised `OSError [Errno 22]` INSIDE the request handler,
killing every request before it served ("connection closed unexpectedly" with zero log lines).
Logging must never take the server down. This pins that log() swallows a broken-stdout write.

Same "source parsers, verify live separately" split as test_bundle_state_gather.py — but
bundle-state-server.py is hyphenated (not a normal import name) and __main__-guarded, so it is loaded
by file path via importlib without starting the HTTP server.
"""
import importlib.util
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
# bundle-state-server.py does `import bundle_state_gather` at module scope, so scripts/ must be
# importable before we exec it.
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

_SPEC = importlib.util.spec_from_file_location(
    "bundle_state_server", _SCRIPTS / "bundle-state-server.py"
)
bss = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bss)  # __name__ != "__main__" -> main()/serve_forever() does NOT run


class _DeadStdout:
    """A stdout whose write/flush raise OSError(EINVAL) — the exact hidden-task dead-pipe shape."""

    def write(self, *_a, **_k):
        raise OSError(22, "Invalid argument")

    def flush(self, *_a, **_k):
        raise OSError(22, "Invalid argument")


def test_log_survives_a_dead_stdout_pipe(monkeypatch):
    # Under the pre-#829 log() (`print(..., flush=True)` with no guard) this raises OSError and
    # would kill the request handler; after the fix it must return normally.
    monkeypatch.setattr(sys, "stdout", _DeadStdout())
    bss.log("hidden-task heartbeat")  # must NOT raise


def test_log_writes_normally_to_a_live_stdout(capsys):
    # The happy path is unchanged: a live stdout still gets the timestamped line.
    bss.log("normal line")
    out = capsys.readouterr().out
    assert "normal line" in out


# ---------------------------------------------------------------------------------------------
# #1222 — newest_obs_log_text must delegate to the bounded head+tail reader, and
# gather_bundle_state must expose an opt-in per-facet timing breakdown (BUNDLE_STATE_TIMING=1)
# so the NEXT session has real data to attack the remaining ~18s cold-log baseline.
# ---------------------------------------------------------------------------------------------

def test_newest_obs_log_text_bounds_a_large_log(tmp_path):
    log_dir = tmp_path / "logs"
    log_dir.mkdir()
    filler = ("X" * 100) + "\n"
    text = "OBS 32.1.2\n" + (filler * 20000) + "TAIL_MARKER\n"  # > 1 MB
    (log_dir / "2026-08-30 00-00-00.txt").write_text(text, encoding="utf-8")

    bounded = bss.newest_obs_log_text(str(log_dir), head_bytes=200, tail_bytes=200)
    assert len(bounded) < len(text)
    assert "OBS 32.1.2" in bounded
    assert "TAIL_MARKER" in bounded


def test_newest_obs_log_text_small_log_returned_whole(tmp_path):
    log_dir = tmp_path / "logs"
    log_dir.mkdir()
    (log_dir / "small.txt").write_text("OBS 32.1.2\n", encoding="utf-8")
    assert bss.newest_obs_log_text(str(log_dir)) == "OBS 32.1.2\n"


def _gather_bundle_state_with_all_externals_stubbed(monkeypatch, tmp_path):
    monkeypatch.setattr(bss, "gather_ndi_inputs", lambda host, password: {})
    log_dir = tmp_path / "logs"
    log_dir.mkdir()
    (log_dir / "obs.txt").write_text("OBS 32.1.2\n", encoding="utf-8")
    return bss.gather_bundle_state(
        "127.0.0.1", "", str(log_dir), str(tmp_path / "missing-ndi.dll"), [],
        genlock_build_sha_file=str(tmp_path / "missing-sha.txt"),
        obs_install_scan_roots=(),
        startup_shortcut=str(tmp_path / "missing.lnk"),
        ahk_path=str(tmp_path / "missing.ahk"),
        obs_dll_path=str(tmp_path / "missing-obs.dll"),
    )


def test_gather_bundle_state_emits_timing_line_when_enabled(monkeypatch, tmp_path, capsys):
    monkeypatch.setenv("BUNDLE_STATE_TIMING", "1")
    _gather_bundle_state_with_all_externals_stubbed(monkeypatch, tmp_path)
    out = capsys.readouterr().out
    assert "gather timing:" in out
    assert "total=" in out


def test_gather_bundle_state_silent_when_timing_disabled(monkeypatch, tmp_path, capsys):
    monkeypatch.delenv("BUNDLE_STATE_TIMING", raising=False)
    _gather_bundle_state_with_all_externals_stubbed(monkeypatch, tmp_path)
    out = capsys.readouterr().out
    assert "gather timing:" not in out


# #1226 — the audio-timeline-lag facet must FLOW through the server's gather (not just the pure
# parser): a lagging `audio-telemetry #800` line in the OBS log must appear as
# audio_ts_lag_ms/audio_ts_lag_src in the served bundle-state dict.
def _gather_with_log(monkeypatch, tmp_path, log_text):
    monkeypatch.setattr(bss, "gather_ndi_inputs", lambda host, password: {})
    log_dir = tmp_path / "logs"
    log_dir.mkdir()
    (log_dir / "obs.txt").write_text(log_text, encoding="utf-8")
    return bss.gather_bundle_state(
        "127.0.0.1", "", str(log_dir), str(tmp_path / "missing-ndi.dll"), [],
        genlock_build_sha_file=str(tmp_path / "missing-sha.txt"),
        obs_install_scan_roots=(),
        startup_shortcut=str(tmp_path / "missing.lnk"),
        ahk_path=str(tmp_path / "missing.ahk"),
        obs_dll_path=str(tmp_path / "missing-obs.dll"),
    )


def test_gather_bundle_state_exposes_audio_ts_lag_facet(monkeypatch, tmp_path):
    log = (
        "OBS 32.1.2 (64-bit, windows)\n"
        "10:44:06.003: audio-telemetry #800 'mbc': ts_lag_ms=1672741 buffered_ms=0 pending=0 timing_adjust_ms=-5\n"
        "10:44:06.004: audio-telemetry #800 'post video': ts_lag_ms=1671003 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    state = _gather_with_log(monkeypatch, tmp_path, log)
    assert state["audio_ts_lag_ms"] == "1672741"
    assert state["audio_ts_lag_src"] == "mbc"


def test_gather_bundle_state_omits_audio_ts_lag_when_no_telemetry(monkeypatch, tmp_path):
    state = _gather_with_log(monkeypatch, tmp_path, "OBS 32.1.2 (64-bit, windows)\n")
    assert "audio_ts_lag_ms" not in state
    assert "audio_ts_lag_src" not in state


# #1231 — the FRESHNESS facet `audio_ts_lag_age_s` (in-log age of the freshest #800 line behind the
# log's newest line) must FLOW through the server's gather from the SAME bounded log_text, so the
# dev1 watchdog can surface a stale-while-log-advancing telemetry stall distinctly.
def test_gather_bundle_state_exposes_audio_age_facet(monkeypatch, tmp_path):
    log = (
        "OBS 32.1.2 (64-bit, windows)\n"
        "10:44:06.003: audio-telemetry #800 'mbc': ts_lag_ms=1672741 buffered_ms=0 pending=0 timing_adjust_ms=-5\n"
        "10:44:06.004: audio-telemetry #800 'post video': ts_lag_ms=1671003 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    state = _gather_with_log(monkeypatch, tmp_path, log)
    # freshest #800 IS the newest log line -> age "0" (telemetry alive); the lag still flows too.
    assert state["audio_ts_lag_age_s"] == "0"
    assert state["audio_ts_lag_ms"] == "1672741"


def test_gather_stale_telemetry_flows_age_and_omits_lag(monkeypatch, tmp_path):
    # (#1231 b) telemetry stopped ~10 min ago while the log kept advancing (a non-#800 line): the age
    # facet flows large and the lag facet is omitted (no FRESH positive reading) -> dev1 -> STALE.
    log = (
        "OBS 32.1.2 (64-bit, windows)\n"
        "10:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=120 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "10:10:00.000: [obs] render tick — the log is alive, telemetry is not\n"
    )
    state = _gather_with_log(monkeypatch, tmp_path, log)
    assert state["audio_ts_lag_age_s"] == "600"
    assert "audio_ts_lag_ms" not in state
