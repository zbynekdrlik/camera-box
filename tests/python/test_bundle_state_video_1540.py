"""Issue 1540 slice 1: the per-connection video counters of one OBS box (`:8899/video-connections.json`).

The follower reads only the new bytes of the newest OBS log on each call, keeps cumulative genlock /
dante counters as their newest value, sums the per-5-s JPEG XS windows, starts over on a new log
file, and moves a connection's `counters_since` when its cumulative counter goes backwards.
"""
import json
import os
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "scripts"))
import bundle_state_video as bsv  # noqa: E402

AUD = ("{t}: genlock-fifo audit 'JXS cam3 src': received={r} consumed=10 underruns={u} holds=0 relocks=0 "
       "dropped_due=2 late_holds=0 locked=1 depth=2 latency_ms=41 (x)\n")
SRC = ("{t}: [distroav] jxs-source 'JXS cam3 src': fps=30.00 frames=150 lost_slices={l} concealed={c} "
       "undecodable=0 decoder_opens=1 camera=3 link_mbps=10000 (issue 1387)\n")
OUT = ("{t}: [distroav] jxs-output 'camera-box JPEG XS program': fps=30.00 frames=150 dropped_busy={d} "
       "offered=150 taken=150 encode_ms_avg/max=28.18/34.43 (issue 1387)\n")
DAN = "{t}: dante-input status: clock=media rx_packets={p} rx_loss=0 rx_gaps=0 rx_holes={h} underruns=0 (x)\n"


def _log(d, name="2026-11-21 02-43-11.txt"):
    p = d / name
    p.write_text("")
    return p


def _append(p, text):
    with open(p, "a") as f:
        f.write(text)
    t = time.time() + len(text)
    os.utime(p, (t, t))


def _conn(snap, name):
    c = [x for x in snap["connections"] if x["sink_input"] == name]
    assert len(c) == 1, snap
    return c[0]


def test_counters_accumulate_across_reads(tmp_path):
    p = _log(tmp_path)
    f = bsv.VideoFollower("strih-snv")
    _append(p, AUD.format(t="02:44:00.000", r=150, u=0) + SRC.format(t="02:44:00.100", l=2, c=1)
            + OUT.format(t="02:44:00.200", d=3) + DAN.format(t="02:44:00.300", p=1000, h=0))
    f.update(str(tmp_path))
    _append(p, AUD.format(t="02:44:05.000", r=300, u=1) + SRC.format(t="02:44:05.100", l=0, c=0)
            + OUT.format(t="02:44:05.200", d=0) + DAN.format(t="02:44:05.300", p=2000, h=64))
    f.update(str(tmp_path))
    s = f.snapshot()
    assert s["schema"] == 1 and s["node"] == "strih-snv" and s["kind"] == "obs"
    assert s["run_id"] == "2026-11-21 02-43-11.txt" and s["run_start"].startswith("2026-11-21T02:43:11")
    c = _conn(s, "JXS cam3 src")
    assert c["source"] == "cam3" and c["kind"] == "jxs" and c["direction"] == "in"
    assert c["frames_received"] == 300 and c["frames_duplicated"] == 1 and c["latency_ms"] == 41
    assert c["slices_lost"] == 2 and c["slices_concealed"] == 1 and c["jxs_frames"] == 300
    assert c["counters_since"].startswith("2026-11-21T02:44:00")
    o = _conn(s, "camera-box JPEG XS program")
    assert o["direction"] == "out" and o["jxs_dropped_busy"] == 3 and o["jxs_frames"] == 300
    dn = _conn(s, "dante-input")
    assert dn["kind"] == "dante" and dn["rx_packets"] == 2000 and dn["rx_holes"] == 64
    assert "frames_duplicated" not in dn
    json.dumps(s)


def test_a_partial_last_line_waits_for_its_newline(tmp_path):
    p = _log(tmp_path)
    f = bsv.VideoFollower("x")
    line = OUT.format(t="02:44:00.200", d=5)
    _append(p, line[:40])
    f.update(str(tmp_path))
    assert f.snapshot()["connections"] == []
    _append(p, line[40:])
    f.update(str(tmp_path))
    assert _conn(f.snapshot(), "camera-box JPEG XS program")["jxs_dropped_busy"] == 5


def test_a_new_log_file_is_a_new_run(tmp_path):
    p = _log(tmp_path)
    f = bsv.VideoFollower("x")
    _append(p, OUT.format(t="02:44:00.200", d=5))
    f.update(str(tmp_path))
    q = _log(tmp_path, "2026-11-21 03-10-00.txt")
    _append(q, OUT.format(t="03:10:05.000", d=1))
    f.update(str(tmp_path))
    s = f.snapshot()
    assert s["run_id"] == "2026-11-21 03-10-00.txt"
    assert _conn(s, "camera-box JPEG XS program")["jxs_dropped_busy"] == 1


def test_a_source_restart_moves_counters_since(tmp_path):
    p = _log(tmp_path)
    f = bsv.VideoFollower("x")
    _append(p, AUD.format(t="02:44:00.000", r=900, u=0) + AUD.format(t="02:44:05.000", r=30, u=0))
    f.update(str(tmp_path))
    c = _conn(f.snapshot(), "JXS cam3 src")
    assert c["frames_received"] == 30 and c["counters_since"].startswith("2026-11-21T02:44:05")


def test_midnight_rolls_the_date(tmp_path):
    p = _log(tmp_path, "2026-11-21 23-59-00.txt")
    f = bsv.VideoFollower("x")
    _append(p, AUD.format(t="23:59:58.000", r=10, u=0) + AUD.format(t="00:00:03.000", r=5, u=0))
    f.update(str(tmp_path))
    assert _conn(f.snapshot(), "JXS cam3 src")["counters_since"].startswith("2026-11-22T00:00:03")


def test_garbage_lines_are_skipped_and_no_log_is_empty(tmp_path):
    f = bsv.VideoFollower("x")
    f.update(str(tmp_path))
    s = f.snapshot()
    assert s["connections"] == [] and s["run_id"] is None
    p = _log(tmp_path)
    _append(p, "02:44:00.000: jxs-output 'x': nonsense\n\xff\xfe broken\n")
    f.update(str(tmp_path))
    assert f.snapshot()["connections"] == []


def _free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def test_the_server_serves_the_route(tmp_path):
    p = _log(tmp_path)
    _append(p, OUT.format(t="02:44:00.200", d=2))
    port = _free_port()
    proc = subprocess.Popen([sys.executable, str(REPO / "scripts" / "bundle-state-server.py"), "--port", str(port),
                             "--obs-log-dir", str(tmp_path)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        body = None
        for _ in range(50):
            try:
                with urllib.request.urlopen("http://127.0.0.1:%d/video-connections.json" % port, timeout=2) as r:
                    body = json.loads(r.read())
                break
            except OSError:
                time.sleep(0.2)
        assert body is not None
        assert body["schema"] == 1
        assert _conn(body, "camera-box JPEG XS program")["jxs_dropped_busy"] == 2
    finally:
        proc.terminate()
        proc.wait(timeout=10)


def test_the_module_is_in_both_deployed_file_lists():
    assert "bundle_state_video.py" in (REPO / "scripts" / "lib" / "bundle-state-files.txt").read_text().split()
    assert "bundle_state_video.py" in (REPO / "scripts" / "lib" / "bundle-state-files.sh").read_text()
