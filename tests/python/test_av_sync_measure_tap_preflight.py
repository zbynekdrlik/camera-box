"""#802 -- unit tests for scripts/av_sync_measure.py's opt-in SRT-tap preflight (--tap-preflight).

The tap is redesigned as an SRT LISTENER on the OBS side (crash-safe); this reader grabs FROM it
as the caller. --tap-preflight short-circuits to a clean NO-SIGNAL (exit 3, #814 family) when the
tap is PROVABLY down, instead of a doomed ffmpeg connect -- and must NEVER false-reject a live or
quiet listener, and must be a no-op when the flag is absent.
"""
import pathlib
import socket
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_measure  # noqa: E402
import srt_tap  # noqa: E402


def _bound_udp():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 0))
    return s, s.getsockname()[1]


class TestTapPreflightHelper:
    def test_no_grab_url_passes(self):
        assert av_sync_measure.tap_preflight(None) == (True, "ok (no --grab)")

    def test_non_srt_url_passes_through(self):
        ok, reason = av_sync_measure.tap_preflight("rtmp://host:1935/app")
        assert ok is True
        assert "skipped" in reason

    def test_live_listener_is_not_false_rejected(self):
        s, port = _bound_udp()
        try:
            ok, reason = av_sync_measure.tap_preflight(f"srt://127.0.0.1:{port}")
            assert ok is True
        finally:
            s.close()

    def test_provably_dead_tap_reported(self, monkeypatch):
        monkeypatch.setattr(
            srt_tap, "reader_should_grab",
            lambda url, **kw: (False, "NO-SIGNAL: nothing listening at x:9998 (tap not up)"))
        ok, reason = av_sync_measure.tap_preflight("srt://x:9998")
        assert ok is False
        assert reason.startswith("NO-SIGNAL:")


class TestMainWiring:
    def test_main_exits_3_on_dead_tap_before_syncnet(self, monkeypatch, capsys):
        # If the tap is provably down, main() must NO-SIGNAL + exit 3 BEFORE the syncnet/ffmpeg
        # presence checks (which would otherwise ERROR-exit for a different reason).
        monkeypatch.setattr(
            av_sync_measure, "tap_preflight",
            lambda grab: (False, "NO-SIGNAL: nothing listening at dev2:9998 (tap not up)"))
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_measure.py", "--grab", "srt://dev2:9998", "--tap-preflight"])
        rc = av_sync_measure.main()
        assert rc == 3
        assert "NO-SIGNAL" in capsys.readouterr().out

    def test_main_does_not_run_preflight_without_the_flag(self, monkeypatch):
        # Without --tap-preflight the preflight must never run (zero behaviour change for the
        # existing #806/#814 flow). We prove it by making tap_preflight explode if called.
        def _boom(grab):
            raise AssertionError("tap_preflight must not run without --tap-preflight")
        monkeypatch.setattr(av_sync_measure, "tap_preflight", _boom)
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_measure.py", "--grab", "srt://dev2:9998", "--repo", "/nonexistent-repo-xyz"])
        # It should fail on the syncnet-repo check (SystemExit), never on the preflight.
        with pytest.raises(SystemExit) as ei:
            av_sync_measure.main()
        assert "syncnet_python repo not found" in str(ei.value)
