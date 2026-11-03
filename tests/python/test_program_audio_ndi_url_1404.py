"""issue 1404 -- the sampler connects each NDI receiver to an address strih-snv reaches on-link
(scripts/program_audio_ndi_url.py).

Read live 3.11.2026: avahi on strih-snv lists every RESOLUME-SNV source on the rig NIC (10.77.9.201)
AND on the issue-1485 audio-VLAN leg (10.77.7.107), whose /24 lives in routing table 70 only. libndi
took the VLAN address for `RESOLUME-SNV (SP-program)` and sat in SYN-SENT for 3 h (connections 0).
The avahi lines below are copied from that read. Hermetic: no avahi, no ip, no libndi is run.
"""
from __future__ import annotations

import ctypes
import pathlib
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import program_audio_ndi as pan  # noqa: E402
import program_audio_ndi_url as pnu  # noqa: E402

SP = "RESOLUME-SNV (SP-program)"
RECREATE_S = getattr(pnu, "RECREATE_AFTER_S", 60.0)
CGOBS = "RESOLUME-SNV (cg-obs)"
AVAHI_3_11 = r"""+;enp7s0.70;IPv4;RESOLUME-SNV\032\040SP-program\041;_ndi._tcp;local
=;enp7s0.70;IPv4;RESOLUME-SNV\032\040cg-obs\041;_ndi._tcp;local;RESOLUME-SNV-000048d4-2217.local;10.77.7.107;5962;"discovery=5960" "groups=Public"
=;enp7s0.70;IPv4;RESOLUME-SNV\032\040SP-program\041;_ndi._tcp;local;RESOLUME-SNV-00001e38-421f.local;10.77.7.107;5961;"discovery=5960" "groups=Public"
=;enp7s0;IPv4;RESOLUME-SNV\032\040cg-obs\041;_ndi._tcp;local;RESOLUME-SNV-000048d4-2217.local;10.77.9.201;5962;"discovery=5960" "groups=Public"
=;enp7s0;IPv4;RESOLUME-SNV\032\040SP-program\041;_ndi._tcp;local;RESOLUME-SNV-00001e38-421f.local;10.77.9.201;5961;"discovery=5960" "groups=Public"
=;enp7s0;IPv4;STREAM-SNV\032\040stream\041;_ndi._tcp;local;STREAM-SNV.local;10.77.9.204;5961;"discovery=5960" "groups=Public"
=;lo;IPv4;STRIH-SNV\032\0402ME\032PGM\041;_ndi._tcp;local;STRIH-SNV.local;127.0.0.1;5961;"discovery=5960" "groups=Public"
"""
ROUTES_3_11 = {
    "10.77.9.201": "10.77.9.201 dev enp7s0 src 10.77.9.202 uid 1000 \\    cache ",
    "10.77.7.107": "10.77.7.107 via 10.77.8.1 dev enp7s0 src 10.77.9.202 uid 1000 \\    cache ",
    "10.77.9.204": "10.77.9.204 dev enp7s0 src 10.77.9.202 uid 1000 \\    cache ",
}


def _run_fake(cmd, _timeout):
    if cmd[0] == "avahi-browse":
        assert cmd == ["avahi-browse", "-tpr", "_ndi._tcp"]
        return AVAHI_3_11
    assert cmd[:5] == ["ip", "-4", "-o", "route", "get"]
    return ROUTES_3_11.get(cmd[5], "")


def test_avahi_names_are_unescaped_from_their_decimal_escapes():
    assert pnu.unescape_avahi(r"RESOLUME-SNV\032\040SP-program\041") == SP
    assert pnu.unescape_avahi(r"STRIH-SNV\032\0402ME\032PGM\041") == "STRIH-SNV (2ME PGM)"
    assert pnu.unescape_avahi(r"a\.b\\c") == "a.b\\c"


def test_only_resolved_ipv4_ndi_records_are_parsed():
    recs = pnu.parse_avahi(AVAHI_3_11 + "=;enp7s0;IPv6;X\\032\\040y\\041;_ndi._tcp;local;h;fe80::1;5961;\n"
                           "=;enp7s0;IPv4;bad;_ndi._tcp;local;h;10.0.0.1;notaport;\n")
    assert pnu.AvahiRecord("enp7s0.70", SP, "10.77.7.107", 5961) in recs
    assert pnu.AvahiRecord("enp7s0", SP, "10.77.9.201", 5961) in recs
    assert len(recs) == 6


def test_the_route_reading():
    assert pnu.on_link_main(ROUTES_3_11["10.77.9.201"])
    assert not pnu.on_link_main(ROUTES_3_11["10.77.7.107"])
    assert not pnu.on_link_main("")
    assert not pnu.on_link_main("unreachable 10.1.1.1 table main")


def test_the_3_11_records_resolve_to_the_rig_nic_address_never_the_vlan_one():
    assert pnu.resolve_url(SP, run=_run_fake) == "10.77.9.201:5961"
    assert pnu.resolve_url(CGOBS, run=_run_fake) == "10.77.9.201:5962"
    assert pnu.resolve_url("STREAM-SNV (stream)", run=_run_fake) == "10.77.9.204:5961"


def test_no_on_link_record_or_no_avahi_leaves_the_name_to_the_sdk():
    assert pnu.resolve_url("RESOLUME-SNV (nothing)", run=_run_fake) is None
    assert pnu.resolve_url(SP, run=lambda cmd, t: "") is None
    vlan_only = [pnu.AvahiRecord("enp7s0.70", SP, "10.77.7.107", 5961)]
    assert pnu.pick_url(SP, vlan_only, lambda a: pnu.on_link_main(ROUTES_3_11[a])) is None


class _Rx:
    made: list = []

    def __init__(self, name, url):
        self.name, self.url, self.conns, self.closed = name, url, 0, False
        _Rx.made.append(self)

    def capture(self, _timeout_ms):
        return None

    def connections(self):
        return self.conns

    def close(self):
        self.closed = True


@pytest.fixture
def clock():
    _Rx.made = []
    return {"t": 1000.0}


def _pinned(clock, urls, logs=None):
    seq = list(urls)
    return pnu.PinnedReceiver(SP, _Rx, resolve=lambda _n: seq.pop(0) if len(seq) > 1 else seq[0],
                              mono=lambda: clock["t"], log=(logs.append if logs is not None else lambda m: None))


def test_the_receiver_is_created_at_the_resolved_url(clock):
    logs = []
    r = _pinned(clock, ["10.77.9.201:5961"], logs)
    assert [(x.name, x.url) for x in _Rx.made] == [(SP, "10.77.9.201:5961")]
    assert r.url == "10.77.9.201:5961" and "at 10.77.9.201:5961" in logs[0]


def test_a_receiver_without_a_connection_for_the_repin_time_is_recreated_at_the_new_url(clock):
    logs = []
    r = _pinned(clock, [None, "10.77.9.201:5961"], logs)
    first = _Rx.made[0]
    for dt in (0.0, 5.0, 10.0):
        clock["t"] = 1000.0 + dt
        r.capture(500)
    assert len(_Rx.made) == 1                       # under REPIN_AFTER_S: nothing
    clock["t"] = 1000.0 + pnu.REPIN_AFTER_S + 0.1
    r.capture(500)
    assert len(_Rx.made) == 2 and _Rx.made[1].url == "10.77.9.201:5961" and first.closed
    assert "re-created after" in logs[-1]
    _Rx.made[1].conns = 1
    assert r.connections() == 1


def test_a_connected_receiver_is_never_recreated(clock):
    r = _pinned(clock, ["10.77.9.201:5961", "10.77.9.201:5999"])
    _Rx.made[0].conns = 1
    for dt in range(0, 100, 5):
        clock["t"] = 1000.0 + dt
        r.capture(500)
    assert len(_Rx.made) == 1


def test_the_same_url_again_keeps_the_receiver(clock):
    r = _pinned(clock, ["10.77.9.201:5961"])
    for dt in range(0, 60, 5):
        clock["t"] = 1000.0 + dt
        r.capture(500)
    assert len(_Rx.made) == 1 and not _Rx.made[0].closed


def test_a_dead_connection_at_the_same_url_is_recreated_bounded_and_logged(clock):
    """Live 3.11.2026 16:51: a cg-obs connection that dropped came back in SYN-SENT to the
    audio-VLAN address and the SDK never left it (connections 0 for 1807 s). With the url
    resolving to the SAME address, the receiver is still re-created there after RECREATE_AFTER_S,
    at most once per RECREATE_AFTER_S, each time logged."""
    assert RECREATE_S == 60.0          # the constant, once it exists, is the one the test runs at
    logs = []
    r = _pinned(clock, ["10.77.9.201:5962"], logs)
    t0 = clock["t"]
    clock["t"] = t0 + 1.0
    r.capture(500)                                    # the connection is gone from here on
    for dt in range(2, int(RECREATE_S), 5):
        clock["t"] = t0 + dt
        r.capture(500)
    assert len(_Rx.made) == 1                         # before RECREATE_AFTER_S: kept
    clock["t"] = t0 + 1.0 + RECREATE_S + 0.4
    r.capture(500)
    assert len(_Rx.made) == 2 and _Rx.made[0].closed and _Rx.made[1].url == "10.77.9.201:5962"
    assert "re-created after 60 s without a connection at 10.77.9.201:5962" in logs[-1]
    for dt in range(1, int(RECREATE_S), 5):  # bounded: not again within RECREATE_AFTER_S
        clock["t"] = t0 + 1.0 + RECREATE_S + 0.4 + dt
        r.capture(500)
    assert len(_Rx.made) == 2
    clock["t"] += RECREATE_S + 1.0
    r.capture(500)
    assert len(_Rx.made) == 3


def test_a_failing_recreate_keeps_the_old_receiver(clock):
    logs = []
    calls = {"n": 0}

    def make(name, url):
        calls["n"] += 1
        if calls["n"] > 1:
            raise RuntimeError("NDIlib_recv_create_v3 failed")
        return _Rx(name, url)

    r = pnu.PinnedReceiver(SP, make, resolve=lambda _n: None if calls["n"] == 0 else "10.77.9.201:5961",
                           mono=lambda: clock["t"], log=logs.append)
    clock["t"] += 1.0
    r.capture(500)
    clock["t"] += pnu.REPIN_AFTER_S + 1.0
    r.capture(500)
    assert not _Rx.made[0].closed and r.url is None and "ERROR re-creating" in logs[-1]


def test_open_receiver_honours_the_pin_switch(monkeypatch):
    _Rx.made = []
    monkeypatch.setenv(pnu.PIN_ENV, "0")
    rx = pnu.open_receiver(SP, _Rx)
    assert isinstance(rx, _Rx) and rx.url is None
    monkeypatch.setenv(pnu.PIN_ENV, "1")
    monkeypatch.setattr(pnu, "resolve_url", lambda _n: "10.77.9.201:5961")
    rx = pnu.open_receiver(SP, _Rx)
    assert isinstance(rx, pnu.PinnedReceiver) and rx.url == "10.77.9.201:5961"


def test_the_sdk_receiver_carries_the_url_into_its_create_settings(monkeypatch):
    seen = {}

    class FakeLib:
        class _F:
            def __init__(self, fn):
                self.fn = fn
                self.restype = self.argtypes = None

            def __call__(self, *a):
                return self.fn(*a)

        def __getattr__(self, name):
            def create(ref):
                s = ref._obj
                seen["name"] = s.source_to_connect_to.p_ndi_name
                seen["url"] = s.source_to_connect_to.p_url_address
                return 1
            fn = {"NDIlib_initialize": lambda: True, "NDIlib_recv_create_v3": create}.get(name, lambda *a: 0)
            f = FakeLib._F(fn)
            setattr(self, name, f)
            return f

    monkeypatch.setattr(pan, "_load_library", lambda _p: FakeLib())
    pan.NdiAudioReceiver(SP, url_address="10.77.9.201:5961")
    assert seen == {"name": SP.encode(), "url": b"10.77.9.201:5961"}
    pan.NdiAudioReceiver(SP)
    assert seen["url"] is None
    assert ctypes.sizeof(pan.NDIlib_source_t) == 2 * ctypes.sizeof(ctypes.c_char_p)


def test_main_creates_every_receiver_through_the_pin(monkeypatch, tmp_path):
    """main(): the CG receivers and the stream one, each at its resolved URL."""
    import program_audio_cg as pcg
    import program_audio_marker as pam
    import program_audio_sampler as pas

    made = []

    class _Sdk:
        def __init__(self, source, lib_path=None, url_address=None):
            made.append((source, url_address))

        def capture(self, _t):
            return None

        def connections(self):
            return 1

        def close(self):
            pass

    class _Dec:
        path, params, built_sha256 = "/fake", {}, "0" * 12

        def __init__(self, _p=None):
            pass

        def sources_stale(self):
            return False

    class _Probe:
        def __init__(self, box):
            self.box, self.home = box, True

        def start(self):
            pass

        def stop(self):
            pass

    monkeypatch.setenv(pnu.PIN_ENV, "1")
    monkeypatch.setattr(pnu, "resolve_url", lambda n: {SP: "10.77.9.201:5961", CGOBS: "10.77.9.201:5962"}.get(n))
    monkeypatch.setattr(pan, "NdiAudioReceiver", _Sdk)
    monkeypatch.setattr(pam, "MarkerDecoder", _Dec)
    monkeypatch.setattr(pcg, "HomeProbe", _Probe)
    for k in (pcg.CG_SOURCES_ENV, pcg.CG_BOX_ENV, "OBS_FLEET_IDENTITY"):
        monkeypatch.delenv(k, raising=False)
    monkeypatch.setattr(pas, "run", lambda receiver, serve_dir, **kw: None)
    assert pas.main(["--serve-dir", str(tmp_path / "serve"), "--source", "STREAM-SNV (stream)",
                     "--http-port", "0"]) == 0
    assert made == [(SP, "10.77.9.201:5961"), (CGOBS, "10.77.9.201:5962"), ("STREAM-SNV (stream)", None)]
