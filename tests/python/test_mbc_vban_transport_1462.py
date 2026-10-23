"""issue 1462 -- rig-mode TEST `#901 item 2` on the Linux stream box.

stream-snv has no Dante soundcard: its OBS input `mbc` is the vendored obs-vban RECEIVE source
(`net.nagater.obs-vban.source`, settings `stream_name` / `port` / `receive_buffer_ms`), fed by
mbc.lan's VBAN stream `stream` on UDP 6980 (the box facts STRIH_PROGRAM_VBAN_*). The #901 check used
to demand `device_id == "Dante Virtual Soundcard (x64)"`, so TEST exited 1 there (live 22.10.2026).
`obs_phase2.py mbc-input-check --vban-stream S --vban-port P` grades the obs-vban source instead:
the right kind, the facts' stream and port, unmuted (mute named first, like the Dante check). The pure
decision is program_audio_vban.mbc_vban_transport_problem (next to the source's one definition).
Without the two flags the Dante check is unchanged (tests/python/test_obs_phase2_mbc_input_check_901.py).
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
_spec = importlib.util.spec_from_file_location("obs_phase2_mbc_vban_1462", _SCRIPTS / "obs_phase2.py")
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_mbc_vban_1462"] = obs_phase2
_spec.loader.exec_module(obs_phase2)
_pav_spec = importlib.util.spec_from_file_location("program_audio_vban_mbc_1462", _SCRIPTS / "program_audio_vban.py")
pav = importlib.util.module_from_spec(_pav_spec)
_pav_spec.loader.exec_module(pav)

KIND = "net.nagater.obs-vban.source"
LIVE = {"ip_from": "", "port": 6980, "receive_buffer_ms": 80, "stream_name": "stream"}  # read off stream-snv 23.10.2026


def problem(kind=KIND, settings=None, muted=False, stream="stream", port=6980):
    return pav.mbc_vban_transport_problem(kind, LIVE if settings is None else settings, muted, stream, port)


def test_obs_phase2_carries_no_second_copy_of_the_decision():
    # the decision lives once, next to the source's definition; obs_phase2 only reads + reports
    assert not hasattr(obs_phase2, "_mbc_vban_transport_problem")
    assert "mbc_vban_transport_problem(" in (_SCRIPTS / "obs_phase2.py").read_text()


# --- the pure decision ----------------------------------------------------------------------------

def test_the_live_stream_snv_input_passes():
    assert problem() is None


def test_the_kind_is_the_one_program_audio_vban_defines():
    assert pav.VBAN_SOURCE_KIND == KIND
    assert problem(kind=pav.VBAN_SOURCE_KIND) is None


def test_a_muted_input_is_named_first_even_with_a_wrong_kind():
    p = problem(kind="pulse_input_capture", muted=True)
    assert p is not None and "MUTED" in p and "pulse_input_capture" not in p


@pytest.mark.parametrize("kind", ["asio_input_capture", "pulse_input_capture", "wasapi_input_capture", ""])
def test_any_other_kind_fails_naming_both_kinds(kind):
    p = problem(kind=kind)
    assert p is not None and KIND in p and repr(kind) in p


def test_another_stream_name_fails_naming_both():
    p = problem(settings=dict(LIVE, stream_name="program"))
    assert p is not None and "'program'" in p and "'stream'" in p


def test_another_port_fails_naming_both():
    p = problem(settings=dict(LIVE, port=6981))
    assert p is not None and "6981" in p and "6980" in p


def test_absent_settings_read_as_the_sources_own_defaults():
    # obs-websocket returns only the keys set on the input: an absent port is the source's 6980,
    # an absent stream name is empty (accept nothing) -- program_audio_vban.settings_differences
    assert problem(settings={"stream_name": "stream"}) is None
    p = problem(settings={"port": 6980})
    assert p is not None and "stream_name" in p


def test_the_receive_buffer_is_not_this_checks_business():
    # R is the operator's setting (verify item 9 NOTEs it); the transport check grades the binding only
    assert problem(settings=dict(LIVE, receive_buffer_ms=40)) is None


# --- the handler over a mocked RPC ----------------------------------------------------------------

class _FakeWS:
    def close(self):
        pass


def _patch(monkeypatch, kind, settings, muted):
    seen = []

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        seen.append((rtype, dict(rdata or {})))
        if rtype == "GetInputSettings":
            return {"inputKind": kind, "inputSettings": settings}
        if rtype == "GetInputMute":
            return {"inputMuted": muted}
        raise AssertionError("unexpected rpc %s (the check is read-only)" % rtype)

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    return seen


def _args(**kw):
    a = dict(host="10.77.9.204", password="", input="mbc", expected_device_id=obs_phase2.DANTE_MBC_DEVICE_ID,
             vban_stream="stream", vban_port=6980)
    a.update(kw)
    return argparse.Namespace(**a)


def test_the_handler_passes_the_live_input_with_two_reads_only(monkeypatch, capsys):
    seen = _patch(monkeypatch, KIND, LIVE, False)
    obs_phase2.mbc_input_check(_args())
    out = capsys.readouterr().out
    assert out.startswith("PASS:") and "VBAN" in out and "'stream'" in out and "6980" in out
    assert [r for r, _ in seen] == ["GetInputSettings", "GetInputMute"]
    assert all(d == {"inputName": "mbc"} for _, d in seen)


def test_the_handler_fails_loud_on_the_dante_era_input(monkeypatch):
    _patch(monkeypatch, "asio_input_capture", {"device_id": "Dante Virtual Soundcard (x64)"}, False)
    with pytest.raises(SystemExit) as e:
        obs_phase2.mbc_input_check(_args())
    msg = str(e.value)
    assert "VBAN" in msg and KIND in msg and "6980" in msg and "issue 1462" in msg


def test_the_handler_fails_loud_when_muted(monkeypatch):
    _patch(monkeypatch, KIND, LIVE, True)
    with pytest.raises(SystemExit, match="MUTED"):
        obs_phase2.mbc_input_check(_args())


def test_without_the_vban_flags_the_dante_check_is_unchanged(monkeypatch, capsys):
    _patch(monkeypatch, KIND, LIVE, False)
    # the obs-vban input carries no device_id: the Dante check (a Windows stream box) still fails it
    with pytest.raises(SystemExit, match="NO device_id"):
        obs_phase2.mbc_input_check(_args(vban_stream=None, vban_port=None))


def test_a_namespace_without_the_new_attributes_runs_the_dante_check(monkeypatch, capsys):
    _patch(monkeypatch, "asio_input_capture", {"device_id": obs_phase2.DANTE_MBC_DEVICE_ID}, False)
    a = argparse.Namespace(host="10.77.9.204", password="", input="mbc", expected_device_id=obs_phase2.DANTE_MBC_DEVICE_ID)
    obs_phase2.mbc_input_check(a)
    assert "Dante transport OK" in capsys.readouterr().out


def test_one_vban_flag_without_the_other_refuses(monkeypatch):
    _patch(monkeypatch, KIND, LIVE, False)
    with pytest.raises(SystemExit, match="go together"):
        obs_phase2.mbc_input_check(_args(vban_port=None))


# --- the CLI --------------------------------------------------------------------------------------

def test_the_cli_parses_the_vban_flags(monkeypatch):
    got = {}
    monkeypatch.setattr(obs_phase2, "mbc_input_check", lambda a: got.update(vars(a)))
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "mbc-input-check", "--host", "10.77.9.204", "--input", "mbc",
                                      "--vban-stream", "stream", "--vban-port", "6980"])
    obs_phase2.main()
    assert got["vban_stream"] == "stream" and got["vban_port"] == 6980


def test_the_cli_defaults_leave_the_vban_mode_off(monkeypatch):
    got = {}
    monkeypatch.setattr(obs_phase2, "mbc_input_check", lambda a: got.update(vars(a)))
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "mbc-input-check", "--host", "h"])
    obs_phase2.main()
    assert got["vban_stream"] is None and got["vban_port"] is None
