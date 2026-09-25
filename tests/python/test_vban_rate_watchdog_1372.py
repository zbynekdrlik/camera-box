"""Issue 1372 part C -- the dev1 VBAN rate watchdog, driven end to end through its capture seam.

scripts/vban-rate-alert-watchdog.sh captures ~60 s of VBAN on strih-lx (the dantesync-disciplined
receiver) and grades every stream ARRIVING there with scripts/vban_rate.py. These tests replace the
ssh capture with a synthetic pcap (VBAN_RATE_CAPTURE_CMD) and run the REAL watchdog in --dry-run: a
confirmed FAULT pages with a time-bucketed key (production-critical: on-air audio), a healthy stream
never pages, the hub's own outgoing streams are never graded, a failed capture is a SKIP.
"""
import importlib.util
import os
import pathlib
import stat
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_WATCHDOG = _ROOT / "scripts" / "vban-rate-alert-watchdog.sh"

_spec = importlib.util.spec_from_file_location("test_vban_rate_1372_builders",
                                               pathlib.Path(__file__).resolve().parent / "test_vban_rate_1372.py")
b = importlib.util.module_from_spec(_spec)
sys.modules["test_vban_rate_1372_builders"] = b
_spec.loader.exec_module(b)

_NOW = 1790349506
_N30S = int(30 * b.NOMINAL / b.SPF)


def _run(tmp_path, pcap_bytes=None, capture_rc=0, **env):
    cap = tmp_path / "cap.pcap"
    if pcap_bytes is not None:
        cap.write_bytes(pcap_bytes)
    stub = tmp_path / "capture.sh"
    stub.write_text("#!/usr/bin/env bash\n"
                    + (f'cp "{cap}" "$1"\n' if pcap_bytes is not None else "")
                    + f"exit {capture_rc}\n")
    stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
    e = {k: v for k, v in os.environ.items() if not k.startswith(("VBAN_RATE_", "OBS_FLEET"))}
    e.update({"VBAN_RATE_CAPTURE_CMD": str(stub), "VBAN_RATE_HOST": "10.77.9.202",
              "VBAN_RATE_CONFIRM_THRESHOLD": "1", "VBAN_RATE_NOW": str(_NOW),
              "VBAN_RATE_ALERT_STATE_DIR": str(tmp_path), "VBAN_RATE_MIN_SPAN_S": "10"})
    e.update(env)
    r = subprocess.run(["bash", str(_WATCHDOG), "--dry-run"], capture_output=True, text=True, env=e)
    assert r.returncode == 0, r.stderr
    return r.stderr


def _inbound(**kw):
    return b.stream_records(_N30S, name="fohabl-strih", src="10.77.7.30", **kw)


def _outbound():
    t0 = 1_790_000_000_000_000_000
    return [(t0 + i * 5_333_333 + 7,
             b.sll2(b.udp_ipv4(src="10.77.9.202", dst="10.77.9.61", payload=b.vban_payload(name="cam1", frame=i))))
            for i in range(_N30S)]


def test_a_lossy_inbound_stream_pages_with_a_bucketed_key(tmp_path):
    err = _run(tmp_path, b.pcap_bytes(276, _inbound(drop={100, 200, 300, 400, 500})))
    assert "fohabl-strih (10.77.7.30 -> strih-lx)" in err and "verdict=FAULT" in err, err
    key = f"vban-rate-fohabl-strih-10_77_7_30-{_NOW // 600}"
    assert f"WOULD alert (dedup-key={key})" in err, err
    assert "strata 5 rámcov" in err


def test_an_off_rate_inbound_stream_pages(tmp_path):
    err = _run(tmp_path, b.pcap_bytes(276, _inbound(ppm=40.0)))
    assert "verdict=FAULT rate_ppm=+40.00" in err, err
    assert "WOULD alert" in err


def test_a_healthy_stream_never_pages_and_outbound_hub_streams_are_not_graded(tmp_path):
    err = _run(tmp_path, b.pcap_bytes(276, sorted(_inbound(ppm=3.0) + _outbound())))
    assert "fohabl-strih" in err and "verdict=OK" in err, err
    assert "cam1 (" not in err, err           # strih-lx -> cam1 is the hub's own output
    assert "WOULD alert" not in err


def test_the_fault_is_confirmed_across_passes_before_paging(tmp_path):
    pcap = b.pcap_bytes(276, _inbound(drop={100, 200, 300}))
    first = _run(tmp_path, pcap, VBAN_RATE_CONFIRM_THRESHOLD="2")
    assert "not yet CONFIRMED" in first and "WOULD alert" not in first
    second = _run(tmp_path, pcap, VBAN_RATE_CONFIRM_THRESHOLD="2")
    assert "WOULD alert" in second
    # recovery is a machine-channel log line, never a page
    third = _run(tmp_path, b.pcap_bytes(276, _inbound()), VBAN_RATE_CONFIRM_THRESHOLD="2")
    assert "RECOVERY: fohabl-strih back inside the bound" in third and "WOULD alert" not in third


def test_a_failed_capture_is_skip_never_a_page(tmp_path):
    err = _run(tmp_path, None, capture_rc=255)
    assert "capture on strih-lx failed or empty -- SKIP" in err
    assert "WOULD alert" not in err


def test_no_vban_arriving_is_logged_not_paged(tmp_path):
    err = _run(tmp_path, b.pcap_bytes(276, _outbound()))
    assert "no VBAN stream arrived" in err and "WOULD alert" not in err


def test_the_capture_command_keeps_the_header_and_filters_vban():
    s = _WATCHDOG.read_text()
    assert 'SNAPLEN="${VBAN_RATE_SNAPLEN:-96}"' in s
    assert "udp[8:4] = 0x5642414e" in s                  # the VBAN magic BPF
    assert "sudo -S -p ''" in s                           # sudo fed on stdin, never a tty prompt
