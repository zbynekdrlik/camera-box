"""Issue 1387, 1.11.2026: the EVENT-mode contract fails when a JPEG XS link is DOWN.

WHY: on the SNV event morning the cameras were moved and rebooted after the rig was set up. Their
JPEG XS senders stayed off, so every strih camera scene was black on air for ~8 min while the
EVENT contract had nothing that looked at JPEG XS (its NDI mapping item passed: the NDI inputs were
fine, just not on program). The owner: the JPEG XS tests must be as good as NDI's or better.

The item `jxs_live` reads, per broadcast OBS box, the newest 5 s window line of every JPEG XS source
and output (`[distroav] jxs-source '<name>': fps=… frames=…` / `jxs-output '<name>': …`) and the
log's newest timestamp. Every link must have delivered frames in a window at most 20 s old. A box
with no window line, an unread box, or a malformed facet fails closed (#1225).

The read is BOUNDED (owner 1.11.2026: a measurement that reads the whole 100+ MB OBS log on a
broadcast box dropped frames; "akekolvek meranie ktore sposobuje vypadky je zle meranie"): only the
last 1 MiB of the newest log (`tail -c`, a seek), at nice 19, and only the last window lines leave
the box.

Tier-0: python + bash only, no rig.
"""
import importlib.util
import json
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "event-jxs-facet.sh"
RIG_MODE = SCRIPTS / "rig-mode.sh"

_spec = importlib.util.spec_from_file_location("event_assert_jxs", SCRIPTS / "event_assert.py")
ea = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ea)


def src(t, name, frames, fps="30.00"):
    return (f"{t}: [distroav] jxs-source '{name}': fps={fps} frames={frames} lost_slices=0 concealed=0 "
            f"undecodable=0 reopens=0 decoder_reopens=0 bpp=5.00 reports=5 (issue 1387)")


def out(t, name, frames):
    return (f"{t}: [distroav] jxs-output '{name}': fps=30.02 frames={frames} dropped_busy=0 offered={frames} "
            f"taken={frames} not_taken=0 refused=0")


STRIH_OK = {"head": "13:30:10.559", "lines": [
    src("13:30:01.100", "JXS cam5 src", 150), src("13:30:02.200", "JXS cam2 src", 151),
    src("13:30:03.300", "JXS cam3 src", 150), out("13:30:06.343", "camera-box JPEG XS program", 151)]}
STREAM_OK = {"head": "13:30:09.000", "lines": [src("13:30:05.000", "JXS PGM src", 150)]}


def test_a_window_line_parses():
    p = ea.parse_jxs_window_line(src("13:30:06.594", "JXS cam5 src", 0, "0.00"))
    assert p == {"t": 13 * 3600 + 30 * 60 + 6.594, "kind": "source", "name": "JXS cam5 src", "frames": 0}
    p = ea.parse_jxs_window_line(out("09:00:00.000", "camera-box JPEG XS program", 151))
    assert p["kind"] == "output" and p["frames"] == 151
    for bad in ("", None, "13:00:00.000: something else", "jxs-source 'x': fps=1 frames=1",
                "13:00:00.000: [distroav] jxs-source 'x': fps=1 frames=x"):
        assert ea.parse_jxs_window_line(bad) is None, bad


def test_every_link_delivering_passes():
    assert ea.jxs_live_ok({"strih": STRIH_OK, "stream": STREAM_OK}) is True


def test_a_source_with_no_frames_fails():
    dead = dict(STRIH_OK, lines=STRIH_OK["lines"] + [src("13:30:08.000", "JXS cam5 src", 0, "0.00")])
    assert ea.jxs_live_ok({"strih": dead, "stream": STREAM_OK}) is False


def test_the_newest_window_decides_a_link_that_came_back():
    back = dict(STRIH_OK, lines=[src("13:29:50.000", "JXS cam5 src", 0, "0.00")] + STRIH_OK["lines"])
    assert ea.jxs_live_ok({"strih": back}) is True


def test_a_link_whose_window_stopped_fails():
    # the output stopped writing 40 s before the log's newest line: it is not running
    stale = {"head": "13:30:50.000", "lines": [src("13:30:45.000", "JXS cam5 src", 150),
                                               out("13:30:10.000", "camera-box JPEG XS program", 151)]}
    assert ea.jxs_live_ok({"strih": stale}) is False


def test_the_age_wraps_at_midnight():
    wrap = {"head": "00:00:03.000", "lines": [src("23:59:58.000", "JXS cam5 src", 150)]}
    assert ea.jxs_live_ok({"strih": wrap}) is True


def test_an_unreadable_facet_fails_closed():
    assert ea.jxs_live_ok(None) is False
    assert ea.jxs_live_ok({}) is False
    assert ea.jxs_live_ok({"strih": STRIH_OK, "stream": None}) is False
    assert ea.jxs_live_ok({"strih": {"head": "13:30:00.000", "lines": []}}) is False, "no link = unknown"
    assert ea.jxs_live_ok({"strih": {"head": "", "lines": STRIH_OK["lines"]}}) is False
    assert ea.jxs_live_ok({"strih": {"head": "13:30:00.000", "lines": "nope"}}) is False
    assert ea.jxs_live_ok(["not", "a", "dict"]) is False


def test_the_detail_names_the_dead_link_and_the_unread_box():
    dead = dict(STRIH_OK, lines=STRIH_OK["lines"] + [src("13:30:08.000", "JXS cam5 src", 0, "0.00")])
    d = ea.jxs_live_detail({"strih": dead, "stream": None})
    assert "strih" in d and "JXS cam5 src" in d and "stream" in d
    assert ea.jxs_live_detail({"strih": STRIH_OK}) == ""


def test_the_item_is_in_the_contract_and_fails_without_its_facet():
    assert "jxs_live" in ea.ITEM_ORDER and "jxs_live" in ea.ITEM_LABELS_SK
    assert ea.compute_item_results({})["jxs_live"] is False
    assert ea.compute_item_results({"jxs_windows": {"strih": STRIH_OK}})["jxs_live"] is True


def test_main_prints_the_dead_link(tmp_path):
    dead = dict(STRIH_OK, lines=STRIH_OK["lines"] + [src("13:30:08.000", "JXS cam5 src", 0, "0.00")])
    facts = tmp_path / "facts.json"
    facts.write_text(json.dumps({"jxs_windows": {"strih": dead, "stream": STREAM_OK}}))
    r = subprocess.run(["python3", str(SCRIPTS / "event_assert.py"), "--facts", str(facts)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 1
    line = next(l for l in r.stdout.splitlines() if ea.ITEM_LABELS_SK["jxs_live"] in l)
    assert line.strip().startswith("[CHYBA]") and "JXS cam5 src" in line, line


def _gather(tmp_path, answers):
    """Run event_jxs_windows_json against a fake sshpass that answers per host."""
    d = tmp_path / "bin"
    d.mkdir()
    (d / "sshpass").write_text(
        "#!/usr/bin/env bash\n"
        'for a in "$@"; do case "$a" in *@*) host="${a#*@}";; esac; done\n'
        'f="$FAKE_DIR/$host"\n'
        '[ -f "$f" ] || exit 255\n'
        'cat "$f"\n')
    (d / "sshpass").chmod(0o755)
    for host, text in answers.items():
        (tmp_path / host).write_text(text)
    script = ('set -euo pipefail\n. "$1"\n'
              'event_jxs_windows_json newlevel pw strih 10.0.0.2 newlevel pw stream 10.0.0.4')
    env = dict(os.environ, PATH=f"{d}:{os.environ['PATH']}", FAKE_DIR=str(tmp_path))
    r = subprocess.run(["bash", "-c", script, "harness", str(LIB)], capture_output=True, text=True,
                       env=env, timeout=60)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


def test_the_gather_reads_each_box_and_marks_an_unreachable_one_null(tmp_path):
    lines = STRIH_OK["lines"]
    got = _gather(tmp_path, {"10.0.0.2": "".join(f"W:{l}\n" for l in lines) + "HEAD:13:30:10.559\n"})
    assert got == {"strih": {"head": "13:30:10.559", "lines": lines}, "stream": None}


def test_the_gather_reads_a_log_without_windows_as_an_empty_box(tmp_path):
    got = _gather(tmp_path, {"10.0.0.2": "HEAD:13:30:10.559\n", "10.0.0.4": "NOLOG\n"})
    assert got == {"strih": {"head": "13:30:10.559", "lines": []}, "stream": {"head": "", "lines": []}}


def _remote(tmp_path, log_text):
    home = tmp_path / "home"
    logs = home / ".config" / "obs-studio" / "logs"
    logs.mkdir(parents=True)
    (logs / "2026-11-01 08-31-53.txt").write_text(log_text)
    cmd = subprocess.run(["bash", "-c", '. "$1"\nevent_jxs_remote_cmd', "harness", str(LIB)],
                         capture_output=True, text=True, timeout=30).stdout
    return subprocess.run(["bash", "-c", cmd], capture_output=True, text=True, timeout=30,
                          env=dict(os.environ, HOME=str(home))).stdout


def test_the_remote_command_reads_only_the_log_tail(tmp_path):
    # a window line older than the bounded tail never leaves the box; the newest ones and the head do
    old = src("08:00:00.000", "JXS old src", 150) + "\n"
    filler = "08:00:01.000: " + "x" * 200 + "\n"
    newest = "".join(l + "\n" for l in STRIH_OK["lines"]) + "13:30:10.559: genlock-fifo audit 'JXS cam5 src': x\n"
    got = _remote(tmp_path, old + filler * 6000 + newest)
    assert "JXS old src" not in got
    assert "W:" + STRIH_OK["lines"][-1] in got
    assert got.rstrip().endswith("HEAD:13:30:10.559")


def test_the_remote_command_is_bounded():
    cmd = subprocess.run(["bash", "-c", '. "$1"\nevent_jxs_remote_cmd', "harness", str(LIB)],
                         capture_output=True, text=True, timeout=30).stdout
    assert "tail -c 1048576" in cmd and "nice -n 19" in cmd
    # every read of the log file is a bounded tail (a seek from the end), never a whole-file read
    reads = [i for i in range(len(cmd)) if cmd.startswith('"$F"', i) and not cmd[:i].endswith("-z ")]
    assert reads, cmd
    for i in reads:
        assert cmd[:i].rstrip().split()[-2:-1] == ["-c"], cmd[max(0, i - 40):i + 4]


def test_rig_mode_gathers_the_facet_into_the_contract():
    s = RIG_MODE.read_text()
    start = s.index("event_mode_assert() {")
    body = s[start:s.index("\n}\n", start)]
    assert "event_jxs_windows_json" in body
    assert '--argjson jxs_windows "$jxs_json"' in body
    assert "jxs_windows: $jxs_windows" in body
    assert "jxs_state_lines" not in body
    assert '. "$RIG_MODE_DIR/lib/event-jxs-facet.sh"' in s
