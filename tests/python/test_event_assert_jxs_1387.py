"""Issue 1387, 1.11.2026: the EVENT-mode contract fails when a JPEG XS link is DOWN.

WHY: on the SNV event morning the cameras were moved and rebooted after the rig was set up. Their
JPEG XS senders stayed off, so every strih camera scene was black on air for ~8 min while the
EVENT contract had nothing that looked at JPEG XS (its NDI mapping item passed: the NDI inputs were
fine, just not on program). The owner: the JPEG XS tests must be as good as NDI's or better.

The new item `jxs_live` reads the newest `jxs-state:` line each broadcast OBS box wrote (the in-OBS
JPEG XS indicator, `GenlockJxsReport`), and passes only when every box's line parses and names no
input or output DOWN. An unreadable box, a log without the line, or a line that does not parse
fails closed (#1225). A box whose inputs only lose slices (DEGRADED, nothing down) passes with the
reason named; black is what the item guards.

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

OK = ("13:06:13.794: jxs-state: state=OK reason=none inputs=3 down=0 degraded=0 outputs=1 down=0 "
      "degraded=0 entry=- (issue 1387)")
IN_DOWN = ("13:15:44.792: jxs-state: state=DEGRADED reason=input_down inputs=3 down=1 degraded=0 outputs=1 "
           "down=0 degraded=0 entry='JXS cam5 src' (issue 1387)")
OUT_DOWN = ("09:00:00.000: jxs-state: state=DOWN reason=output_down inputs=3 down=0 degraded=0 outputs=1 "
            "down=1 degraded=0 entry='camera-box JPEG XS program' (issue 1387)")
SLICES = ("09:44:29.031: jxs-state: state=DEGRADED reason=input_degraded inputs=1 down=0 degraded=1 "
          "outputs=0 down=0 degraded=0 entry='JXS PGM src' (issue 1387)")
OFF = "08:00:00.000: jxs-state: state=OFF reason=none inputs=0 down=0 degraded=0 outputs=0 down=0 degraded=0 entry=- (issue 1387)"


def test_the_line_parses_into_its_counts():
    p = ea.parse_jxs_state_line(IN_DOWN)
    assert p == {"state": "DEGRADED", "reason": "input_down", "inputs": 3, "inputs_down": 1,
                 "inputs_degraded": 0, "outputs": 1, "outputs_down": 0, "outputs_degraded": 0,
                 "entry": "JXS cam5 src"}
    assert ea.parse_jxs_state_line(OK)["entry"] == ""
    for bad in ("", "jxs-state: state=OK", "13:00:00: something else", None, "jxs-state: state=OK reason=none "
                "inputs=x down=0 degraded=0 outputs=1 down=0 degraded=0 entry=- (issue 1387)"):
        assert ea.parse_jxs_state_line(bad) is None, bad


def test_every_link_up_passes():
    assert ea.jxs_live_ok({"strih-snv": OK, "stream-snv": SLICES}) is True
    assert ea.jxs_live_ok({"strih-snv": OFF}) is True, "a box without any JPEG XS row is not down"


def test_an_input_or_output_down_fails():
    assert ea.jxs_live_ok({"strih-snv": IN_DOWN, "stream-snv": OK}) is False
    assert ea.jxs_live_ok({"strih-snv": OUT_DOWN, "stream-snv": OK}) is False


def test_an_unreadable_facet_fails_closed():
    assert ea.jxs_live_ok(None) is False
    assert ea.jxs_live_ok({}) is False
    assert ea.jxs_live_ok({"strih-snv": OK, "stream-snv": None}) is False
    assert ea.jxs_live_ok({"strih-snv": OK, "stream-snv": ""}) is False, "no line = unknown"
    assert ea.jxs_live_ok({"strih-snv": "garbage"}) is False
    assert ea.jxs_live_ok(["not", "a", "dict"]) is False


def test_the_detail_names_the_dead_link_and_the_unread_box():
    d = ea.jxs_live_detail({"strih-snv": IN_DOWN, "stream-snv": None})
    assert "strih-snv" in d and "JXS cam5 src" in d and "stream-snv" in d
    assert ea.jxs_live_detail({"strih-snv": OK}) == ""


def test_the_item_is_in_the_contract_and_fails_without_its_facet():
    assert "jxs_live" in ea.ITEM_ORDER and "jxs_live" in ea.ITEM_LABELS_SK
    assert ea.compute_item_results({})["jxs_live"] is False
    assert ea.compute_item_results({"jxs_state_lines": {"strih-snv": OK}})["jxs_live"] is True


def test_main_prints_the_dead_link(tmp_path):
    facts = tmp_path / "facts.json"
    facts.write_text(json.dumps({"jxs_state_lines": {"strih-snv": IN_DOWN, "stream-snv": OK}}))
    r = subprocess.run(["python3", str(SCRIPTS / "event_assert.py"), "--facts", str(facts)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 1
    line = next(l for l in r.stdout.splitlines() if ea.ITEM_LABELS_SK["jxs_live"] in l)
    assert line.strip().startswith("[CHYBA]") and "JXS cam5 src" in line, line


def _gather(tmp_path, answers):
    """Run event_jxs_state_lines_json against a fake sshpass that answers per host."""
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
              'event_jxs_state_lines_json newlevel pw strih-snv 10.0.0.2 newlevel pw stream-snv 10.0.0.4')
    env = dict(os.environ, PATH=f"{d}:{os.environ['PATH']}", FAKE_DIR=str(tmp_path))
    r = subprocess.run(["bash", "-c", script, "harness", str(LIB)], capture_output=True, text=True,
                       env=env, timeout=60)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


def test_the_gather_reads_each_box_and_marks_an_unreachable_one_null(tmp_path):
    got = _gather(tmp_path, {"10.0.0.2": "LINE:" + IN_DOWN + "\n"})
    assert got == {"strih-snv": IN_DOWN, "stream-snv": None}


def test_the_gather_tells_no_line_and_no_log_apart_from_a_dead_box(tmp_path):
    got = _gather(tmp_path, {"10.0.0.2": "LINE:\n", "10.0.0.4": "NOLOG\n"})
    assert got == {"strih-snv": "", "stream-snv": ""}


def test_the_remote_command_reads_the_newest_log_line():
    script = '. "$1"\nevent_jxs_line_remote_cmd'
    r = subprocess.run(["bash", "-c", script, "harness", str(LIB)], capture_output=True, text=True, timeout=30)
    cmd = r.stdout
    assert "ls -t" in cmd and ".config/obs-studio/logs" in cmd and "jxs-state: state=" in cmd
    assert "tail -n 1" in cmd


def test_rig_mode_gathers_the_facet_into_the_contract():
    s = RIG_MODE.read_text()
    start = s.index("event_mode_assert() {")
    body = s[start:s.index("\n}\n", start)]
    assert "event_jxs_state_lines_json" in body
    assert '--argjson jxs_state_lines "$jxs_json"' in body
    assert "jxs_state_lines: $jxs_state_lines" in body
    assert '. "$RIG_MODE_DIR/lib/event-jxs-facet.sh"' in s
