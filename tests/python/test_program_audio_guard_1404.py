"""issue 1404 Task 2 -- `scripts/program_audio_guard.py`, the CLI both YouTube gates call
(camera-box and restreamer issue 357) before and during a broadcast.

Contract: exit 0 MEASUREMENT / SILENT (fresh), 1 FOREIGN, 2 UNKNOWN / stale / unreachable /
unreadable -- fail closed; a MEASUREMENT without a marker chain (a sampler older than the marker
requirement, ROZHODNUTÉ 6026826572) is UNKNOWN. One stdout line:
  program-audio verdict=<V> rms=<x> outside_band=<y>% age=<s> markers=<n> chain=<c>[ reason=<...>]

Run as a real subprocess (real exit codes) against a real stdlib HTTP server on an ephemeral
127.0.0.1 port, and once end-to-end through the sampler's real endpoint (program_audio_http).
"""
from __future__ import annotations

import http.server
import json
import pathlib
import re
import socket
import subprocess
import sys
import threading
from datetime import datetime, timedelta, timezone

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import program_audio_http as pah  # noqa: E402
import rig_serve_files as rsf  # noqa: E402

GUARD = _SCRIPTS / "program_audio_guard.py"
LINE = re.compile(
    r"^program-audio verdict=(MEASUREMENT|FOREIGN|SILENT|UNKNOWN) rms=(-?\d+\.\d|-) "
    r"outside_band=(\d+\.\d|-)% age=(-?\d+\.\d|-) markers=(\d+|-) chain=(\d+|-)( reason=.+)?$"
)


class _Fake:
    """Serves one canned response on any GET."""

    def __init__(self, status=200, body=b"", content_type="application/json"):
        outer = self

        class H(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802
                outer.paths.append(self.path)
                self.send_response(outer.status)
                self.send_header("Content-Type", outer.content_type)
                self.send_header("Content-Length", str(len(outer.body)))
                self.end_headers()
                self.wfile.write(outer.body)

            def log_message(self, *a):
                pass

        self.status, self.body, self.content_type, self.paths = status, body, content_type, []
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/program-audio.json"
        self._t = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self._t.start()
        return self

    def __exit__(self, *exc):
        self.server.shutdown()
        self.server.server_close()
        self._t.join(timeout=5)


def _payload(verdict="MEASUREMENT", age=1.0, rms=-35.6, outside=16.8, markers=9, chain=8, **extra):
    p = {"schema": 1, "ts_utc": rsf.format_ts_utc(datetime.now(timezone.utc)), "age_s": age,
         "verdict": verdict, "rms_dbfs": rms, "outside_band_pct": outside, "window_s": 2.0,
         "source": "STREAM-SNV (stream)", "last_foreign_ts_utc": None, "last_foreign_age_s": None,
         "markers_decoded": markers, "marker_chain": chain}
    p.update(extra)
    return json.dumps(p).encode()


def _guard(url, *extra):
    r = subprocess.run([sys.executable, str(GUARD), "--url", url, "--max-age", "10", *extra],
                       capture_output=True, text=True, timeout=30)
    lines = r.stdout.splitlines()
    assert len(lines) == 1, r.stdout + r.stderr
    m = LINE.match(lines[0])
    assert m, lines[0]
    return r.returncode, lines[0], m


def test_fresh_measurement_exits_0_with_the_contract_line():
    with _Fake(body=_payload("MEASUREMENT", age=1.3)) as f:
        rc, line, _m = _guard(f.url)
    assert rc == 0
    assert line == "program-audio verdict=MEASUREMENT rms=-35.6 outside_band=16.8% age=1.3 markers=9 chain=8"
    assert f.paths == ["/program-audio.json"]


def test_fresh_silent_exits_0():
    with _Fake(body=_payload("SILENT", rms=-92.4, outside=None, markers=None, chain=None)) as f:
        rc, line, _m = _guard(f.url)
    assert rc == 0
    assert line == "program-audio verdict=SILENT rms=-92.4 outside_band=-% age=1.0 markers=- chain=-"


def test_foreign_exits_1():
    with _Fake(body=_payload("FOREIGN", rms=-18.2, outside=78.5, markers=31, chain=1)) as f:
        rc, line, _m = _guard(f.url)
    assert rc == 1
    assert line == "program-audio verdict=FOREIGN rms=-18.2 outside_band=78.5% age=1.0 markers=31 chain=1"


def test_a_stale_foreign_still_exits_1_never_downgraded():
    with _Fake(body=_payload("FOREIGN", age=60.0)) as f:
        rc, line, _m = _guard(f.url)
    assert rc == 1
    assert "stale" in line


def test_unknown_exits_2_with_its_reason():
    with _Fake(body=_payload("UNKNOWN", rms=None, outside=None, reason="no audio for 6.0 s")) as f:
        rc, line, m = _guard(f.url)
    assert rc == 2
    assert m.group(1) == "UNKNOWN"
    assert "no audio for 6.0 s" in line


def test_a_stale_measurement_exits_2():
    with _Fake(body=_payload("MEASUREMENT", age=42.0)) as f:
        rc, line, m = _guard(f.url)
    assert rc == 2
    assert m.group(1) == "UNKNOWN"
    assert m.group(4) == "42.0"
    assert "stale" in line and "MEASUREMENT" in line


def test_a_measurement_exactly_at_max_age_is_fresh():
    with _Fake(body=_payload("MEASUREMENT", age=10.0)) as f:
        rc, _line, _m = _guard(f.url)
    assert rc == 0


def test_a_measurement_without_a_marker_chain_exits_2():
    """A payload from a sampler older than the marker requirement says MEASUREMENT on the spectral
    share alone: never trusted (ROZHODNUTÉ 6026826572)."""
    p = json.loads(_payload("MEASUREMENT"))
    del p["marker_chain"], p["markers_decoded"]
    with _Fake(body=json.dumps(p).encode()) as f:
        rc, line, m = _guard(f.url)
    assert rc == 2
    assert m.group(1) == "UNKNOWN"
    assert "marker chain" in line
    with _Fake(body=_payload("MEASUREMENT", chain=None)) as f:
        rc2, _l2, m2 = _guard(f.url)
    assert rc2 == 2 and m2.group(1) == "UNKNOWN"


def test_silent_and_foreign_never_need_a_marker_chain():
    with _Fake(body=_payload("SILENT", rms=-90.0, outside=None, markers=None, chain=None)) as f:
        rc, _line, _m = _guard(f.url)
    assert rc == 0
    with _Fake(body=_payload("FOREIGN", markers=None, chain=None)) as f:
        rc2, _l2, _m2 = _guard(f.url)
    assert rc2 == 1


def test_the_line_carries_the_marker_counts():
    with _Fake(body=_payload("MEASUREMENT", markers=12, chain=7)) as f:
        _rc, _line, m = _guard(f.url)
    assert (m.group(5), m.group(6)) == ("12", "7")


def test_a_missing_age_exits_2():
    with _Fake(body=_payload("MEASUREMENT", age=None)) as f:
        rc, _line, m = _guard(f.url)
    assert rc == 2
    assert m.group(4) == "-"


def test_an_unknown_verdict_string_exits_2():
    with _Fake(body=_payload("LOUD")) as f:
        rc, _line, m = _guard(f.url)
    assert rc == 2 and m.group(1) == "UNKNOWN"


def test_http_404_exits_2():
    with _Fake(status=404, body=b"") as f:
        rc, line, _m = _guard(f.url)
    assert rc == 2
    assert "404" in line


def test_invalid_json_exits_2():
    with _Fake(body=b"{nope") as f:
        rc, _line, _m = _guard(f.url)
    assert rc == 2


def test_unreachable_exits_2():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()  # nothing listens there
    rc, line, m = _guard(f"http://127.0.0.1:{port}/program-audio.json", "--timeout", "2")
    assert rc == 2
    assert m.group(1) == "UNKNOWN"
    assert "unreachable" in line


def test_defaults_point_at_strih_lx():
    # issue 1404 ROZHODNUTE 6039368611: the sampler moved off dev1 to strih-lx and serves :8891 itself.
    src = GUARD.read_text(encoding="utf-8")
    assert 'DEFAULT_URL = "http://10.77.9.202:8891/program-audio.json"' in src
    assert "DEFAULT_MAX_AGE_S = 10.0" in src


def test_end_to_end_through_the_real_sampler_endpoint(tmp_path):
    """The guard against the sampler's own endpoint (program_audio_http, strih-lx :8891), the one
    place the verdict is served since the dev1 lease route was retired (issue 1404, 8.10.2026)."""
    serve = tmp_path / "serve"
    serve.mkdir()
    payload = json.loads(_payload("MEASUREMENT", age=0.0))
    payload["ts_utc"] = rsf.format_ts_utc(datetime.now(timezone.utc) - timedelta(seconds=3))
    rsf.write_bytes_atomic(str(serve / rsf.PROGRAM_AUDIO_NAME), json.dumps(payload).encode())
    server = pah.make_server("127.0.0.1", 0, str(serve))
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    try:
        url = f"http://127.0.0.1:{server.server_address[1]}/program-audio.json"
        rc, line, m = _guard(url)
        assert rc == 0, line
        assert 2.5 <= float(m.group(4)) <= 6.0  # the server's age, not the file's 0.0
        payload["ts_utc"] = rsf.format_ts_utc(datetime.now(timezone.utc) - timedelta(seconds=30))
        rsf.write_bytes_atomic(str(serve / rsf.PROGRAM_AUDIO_NAME), json.dumps(payload).encode())
        rc2, _line2, _m2 = _guard(url)
        assert rc2 == 2  # a sampler that stopped writing reads stale, never fresh
    finally:
        server.shutdown()
        server.server_close()
        t.join(timeout=5)


# ---------------------------------------------------------------------------------------------
# review round 1 (issue 1404 T2): the FOREIGN latch, negative ages, broken HTTP
# ---------------------------------------------------------------------------------------------


def test_a_foreign_window_since_the_last_poll_exits_1_even_when_the_current_one_is_clean():
    with _Fake(body=_payload("MEASUREMENT", last_foreign_age_s=6.0)) as f:
        rc, line, m = _guard(f.url)
    assert rc == 1
    assert m.group(1) == "FOREIGN"
    assert "latched" in line and "6.0 s ago" in line


def test_a_foreign_window_older_than_the_latch_hold_no_longer_trips():
    with _Fake(body=_payload("MEASUREMENT", last_foreign_age_s=40.0)) as f:
        rc, _line, _m = _guard(f.url)
    assert rc == 0


def test_the_latch_hold_outlasts_max_age_so_a_late_poll_still_sees_it():
    """A gate polls every ~10 s plus the guard's own runtime: a FOREIGN window that ended 25 s
    ago is still reported (latch hold 30 s, independent of --max-age 10)."""
    with _Fake(body=_payload("MEASUREMENT", last_foreign_age_s=25.0)) as f:
        rc, line, _m = _guard(f.url)
    assert rc == 1 and "latched" in line
    with _Fake(body=_payload("MEASUREMENT", last_foreign_age_s=25.0)) as f:
        rc2, _l2, _m2 = _guard(f.url, "--latch-s", "20")
    assert rc2 == 0


def test_the_latch_hold_default_is_pinned():
    src = GUARD.read_text(encoding="utf-8")
    assert "DEFAULT_LATCH_S = 30.0" in src


def test_a_negative_age_beyond_a_clock_step_is_stale():
    with _Fake(body=_payload("MEASUREMENT", age=-3600.0)) as f:
        rc, line, m = _guard(f.url)
    assert rc == 2
    assert m.group(1) == "UNKNOWN" and "stale" in line


def test_a_small_negative_age_from_a_date_step_is_fresh():
    with _Fake(body=_payload("MEASUREMENT", age=-0.4)) as f:
        rc, _line, _m = _guard(f.url)
    assert rc == 0


class _RawServer:
    """Answers every connection with fixed raw bytes, then closes (a broken HTTP peer)."""

    def __init__(self, raw: bytes):
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(4)
        self.url = f"http://127.0.0.1:{self.sock.getsockname()[1]}/program-audio.json"
        self.raw = raw
        self._t = threading.Thread(target=self._serve, daemon=True)

    def _serve(self):
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            with conn:
                conn.recv(4096)
                conn.sendall(self.raw)

    def __enter__(self):
        self._t.start()
        return self

    def __exit__(self, *exc):
        self.sock.shutdown(socket.SHUT_RDWR)
        self.sock.close()
        self._t.join(timeout=5)


def test_a_garbled_status_line_exits_2_with_the_line():
    with _RawServer(b"garbage\r\n\r\n") as srv:
        rc, line, m = _guard(srv.url)
    assert rc == 2
    assert m.group(1) == "UNKNOWN"
    assert "broken HTTP response" in line  # named, not the generic guard-error catch-all


def test_a_truncated_body_exits_2_with_the_line():
    raw = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n{\"verdict\": "
    with _RawServer(raw) as srv:
        rc, line, m = _guard(srv.url)
    assert rc == 2
    assert m.group(1) == "UNKNOWN"
    assert "broken HTTP response" in line
