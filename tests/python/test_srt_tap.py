"""#802 -- unit tests for scripts/srt_tap.py: the SRT-tap launch-path guard + listener-mode
redesign that keeps the A/V-sync tap from ever crashing OBS.

The load-bearing test is `test_caller_to_the_crash_url_is_refused`: the EXACT 2026-07-19 crash
URL (`srt://127.0.0.1:9998`, a caller with no `mode=`) must be REFUSED by assert_safe_to_start().
A no-op guard (the RED baseline) does not raise -> that test fails -> it reproduces the
unguarded, crash-enabling condition. The GREEN implementation refuses it.

Pure stdlib; the socket probes are exercised against real loopback UDP sockets on ephemeral
ports (deterministic directions only -- a bound listener never sends ICMP port-unreachable).
"""
import pathlib
import socket
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import srt_tap  # noqa: E402


# ---------------------------------------------------------------------------
# parse_srt_target
# ---------------------------------------------------------------------------
class TestParseSrtTarget:
    def test_host_and_port(self):
        assert srt_tap.parse_srt_target("srt://127.0.0.1:9998") == ("127.0.0.1", 9998)

    def test_host_and_port_with_query(self):
        assert srt_tap.parse_srt_target("srt://dev2:9998?mode=listener&latency=120") == ("dev2", 9998)

    def test_non_srt_scheme_raises(self):
        with pytest.raises(ValueError):
            srt_tap.parse_srt_target("rtmp://host:1935/app")

    def test_missing_port_raises(self):
        with pytest.raises(ValueError):
            srt_tap.parse_srt_target("srt://127.0.0.1")


# ---------------------------------------------------------------------------
# srt_mode / is_listener_url
# ---------------------------------------------------------------------------
class TestSrtMode:
    def test_default_is_caller(self):
        # libsrt/ffmpeg convention: no mode= -> caller (the crash-prone mode).
        assert srt_tap.srt_mode("srt://127.0.0.1:9998") == "caller"

    def test_explicit_listener(self):
        assert srt_tap.srt_mode("srt://0.0.0.0:9998?mode=listener") == "listener"

    def test_explicit_caller(self):
        assert srt_tap.srt_mode("srt://dev2:9998?mode=caller") == "caller"

    def test_rendezvous(self):
        assert srt_tap.srt_mode("srt://dev2:9998?mode=rendezvous") == "rendezvous"

    def test_mode_value_case_insensitive(self):
        assert srt_tap.srt_mode("srt://0.0.0.0:9998?mode=LISTENER") == "listener"

    def test_mode_key_case_insensitive(self):
        assert srt_tap.srt_mode("srt://0.0.0.0:9998?MODE=listener") == "listener"

    def test_is_listener_url(self):
        assert srt_tap.is_listener_url("srt://0.0.0.0:9998?mode=listener") is True
        assert srt_tap.is_listener_url("srt://127.0.0.1:9998") is False
        assert srt_tap.is_listener_url("srt://dev2:9998?mode=caller") is False


# ---------------------------------------------------------------------------
# recommend_tap_url
# ---------------------------------------------------------------------------
class TestRecommendTapUrl:
    def test_default_is_a_listener(self):
        url = srt_tap.recommend_tap_url()
        assert srt_tap.is_listener_url(url)
        assert srt_tap.parse_srt_target(url) == ("0.0.0.0", srt_tap.SRT_TAP_DEFAULT_PORT)

    def test_custom_port(self):
        url = srt_tap.recommend_tap_url(port=12345)
        assert srt_tap.parse_srt_target(url)[1] == 12345
        assert srt_tap.is_listener_url(url)

    def test_extra_params_appended_and_mode_forced(self):
        # a caller-supplied mode= must NOT be able to override the enforced listener mode.
        url = srt_tap.recommend_tap_url(extra_params={"latency": 120, "mode": "caller"})
        assert srt_tap.is_listener_url(url)
        assert "latency=120" in url


# ---------------------------------------------------------------------------
# assert_safe_to_start -- THE launch-path guard
# ---------------------------------------------------------------------------
class TestAssertSafeToStart:
    def test_caller_to_the_crash_url_is_refused(self):
        # The EXACT 2026-07-19 crash URL: an SRT caller (no mode=) to a local port. Starting an
        # output like this against an unreachable listener crashed OBS. The guard MUST refuse it.
        with pytest.raises(srt_tap.UnsafeTapError):
            srt_tap.assert_safe_to_start("srt://127.0.0.1:9998")

    def test_explicit_caller_is_refused(self):
        with pytest.raises(srt_tap.UnsafeTapError):
            srt_tap.assert_safe_to_start("srt://dev2:9998?mode=caller")

    def test_rendezvous_is_refused(self):
        with pytest.raises(srt_tap.UnsafeTapError):
            srt_tap.assert_safe_to_start("srt://dev2:9998?mode=rendezvous")

    def test_listener_is_safe(self):
        # A listener bind never fails on a missing peer -> the crash trigger cannot occur.
        assert srt_tap.assert_safe_to_start("srt://0.0.0.0:9998?mode=listener") is None

    def test_refusal_message_hands_back_a_listener_suggestion(self):
        with pytest.raises(srt_tap.UnsafeTapError) as ei:
            srt_tap.assert_safe_to_start("srt://127.0.0.1:9998")
        msg = str(ei.value)
        assert "mode=listener" in msg
        # the suggested URL keeps the same port
        assert ":9998" in msg

    def test_malformed_url_raises_valueerror(self):
        with pytest.raises(ValueError):
            srt_tap.assert_safe_to_start("srt://127.0.0.1")  # no port


# ---------------------------------------------------------------------------
# socket probes -- deterministic loopback directions only
# ---------------------------------------------------------------------------
def _free_udp_port():
    """Bind an ephemeral loopback UDP port, return (socket, port). Caller owns/closes it."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 0))
    return s, s.getsockname()[1]


class TestProbeListenerBindable:
    def test_free_port_is_bindable(self):
        s, port = _free_udp_port()
        s.close()  # release it so probe can bind
        assert srt_tap.probe_listener_bindable("127.0.0.1", port) is True

    def test_port_already_held_is_not_bindable(self):
        s, port = _free_udp_port()
        try:
            # something already holds the port -> probe cannot bind -> False (a listener is up).
            assert srt_tap.probe_listener_bindable("127.0.0.1", port) is False
        finally:
            s.close()

    def test_non_local_address_cannot_confirm_returns_none(self):
        # binding a non-local address fails with EADDRNOTAVAIL, not EADDRINUSE -> None (unknown).
        assert srt_tap.probe_listener_bindable("203.0.113.1", 9998) is None


class TestProbeUdpPortRefused:
    def test_bound_listener_is_never_reported_refused(self):
        # A bound UDP socket never emits ICMP port-unreachable -> probe must return False
        # (this direction is deterministic on every platform).
        s, port = _free_udp_port()
        try:
            assert srt_tap.probe_udp_port_refused("127.0.0.1", port, timeout=0.5) is False
        finally:
            s.close()

    def test_returns_a_bool_and_never_raises(self):
        s, port = _free_udp_port()
        s.close()  # nothing listening now
        result = srt_tap.probe_udp_port_refused("127.0.0.1", port, timeout=0.5)
        assert isinstance(result, bool)


# ---------------------------------------------------------------------------
# reader_should_grab -- the caller/reader side
# ---------------------------------------------------------------------------
class TestReaderShouldGrab:
    def test_non_srt_url_is_passed_through(self):
        ok, reason = srt_tap.reader_should_grab("rtmp://host:1935/app")
        assert ok is True
        assert "skipped" in reason

    def test_live_listener_is_not_false_rejected(self):
        # a bound listener does not answer our probe bytes, but must NOT be rejected: only a
        # PROVABLY dead tap short-circuits (never a live-but-quiet listener).
        s, port = _free_udp_port()
        try:
            ok, reason = srt_tap.reader_should_grab(f"srt://127.0.0.1:{port}", timeout=0.5)
            assert ok is True
            assert reason == "ok"
        finally:
            s.close()

    def test_returns_tuple_bool_str(self):
        s, port = _free_udp_port()
        s.close()
        ok, reason = srt_tap.reader_should_grab(f"srt://127.0.0.1:{port}", timeout=0.5)
        assert isinstance(ok, bool)
        assert isinstance(reason, str)
