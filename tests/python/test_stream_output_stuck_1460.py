"""issue 1460 (owner ROZHODNUTÉ 12.11.2026): the stuck-output page.

"na discord do alerts keď je scéna PRO na stream obs a je zapnuté streamovanie, vtedy má prísť správa
do discordu, že sa zasekol výstup" -- page ONLY while the stream OBS programs the production scene AND
streams AND its output is stuck; never outside that state.

Tier-0: the pure decision, the probe's printed lines, and the watchdog run in --dry-run through its
STREAM_OUTPUT_STUCK_FETCH_CMD replay seam (no network: the fetch is a fixture script, the host a
TEST-NET address).
"""
import importlib.util
import os
import pathlib
import stat
import subprocess
import sys

_REPO = pathlib.Path(__file__).resolve().parents[2]
_WATCHDOG = _REPO / "scripts" / "stream-output-stuck-alert-watchdog.sh"
_DECIDE = _REPO / "scripts" / "stream_output_stuck_decision.py"
_PROBE = _REPO / "scripts" / "stream_output_stuck_probe.py"
_SCENE_LIB = _REPO / "scripts" / "lib" / "stream-dev-scene.sh"
_UNITS = _REPO / "systemd"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


sod = _load("stream_output_stuck_decision_1460", _DECIDE)


def _production_scene():
    """The production scene name as the lib declares it -- never retyped in this test."""
    r = subprocess.run(["bash", "-c", '. "$1"; printf %s "$STREAM_PRODUCTION_SCENE_DEFAULT"', "t",
                        str(_SCENE_LIB)], capture_output=True, text=True, check=True)
    assert r.stdout, "the lib declares no production scene"
    return r.stdout


PRO = _production_scene()


def _body(scene=None, a0=1, a1=1, render=(100, 190), frames=(50, 140), bytes_=(1000, 900000),
          rec=(0, 0)):
    scene = PRO if scene is None else scene
    lines = [f"scene={scene}"]
    for i in (0, 1):
        lines += [f"active{i}={(a0, a1)[i]}", f"render{i}={render[i]}", f"frames{i}={frames[i]}",
                  f"bytes{i}={bytes_[i]}", f"reconnecting{i}={rec[i]}"]
    lines.append("window_s=3.0")
    return "\n".join(lines) + "\n"


def _verdict(body, reachable=1, scene=None):
    return sod.classify(sod.parse_probe(body), reachable, PRO if scene is None else scene)


# --- the pure decision ----------------------------------------------------------------------------

def test_a_live_production_stream_is_ok():
    assert _verdict(_body()) == ("OK", [])


def test_each_frozen_counter_is_a_named_reason():
    assert _verdict(_body(render=(100, 100))) == ("STUCK", ["render"])
    assert _verdict(_body(frames=(50, 50))) == ("STUCK", ["encode"])
    assert _verdict(_body(bytes_=(1000, 1000))) == ("STUCK", ["bytes"])
    assert _verdict(_body(rec=(0, 1))) == ("STUCK", ["reconnecting"])
    assert _verdict(_body(render=(1, 1), frames=(2, 2), bytes_=(3, 3))) == (
        "STUCK", ["render", "encode", "bytes"])


def test_another_program_scene_never_pages_even_when_stuck():
    stuck = dict(render=(1, 1), frames=(2, 2), bytes_=(3, 3))
    assert _verdict(_body(scene="Development", **stuck))[0] == "NOT_PRODUCTION"
    assert _verdict(_body(scene=PRO.lower(), **stuck))[0] == "NOT_PRODUCTION"


def test_not_streaming_in_either_snapshot_never_pages():
    stuck = dict(render=(1, 1), frames=(2, 2), bytes_=(3, 3))
    assert _verdict(_body(a0=0, a1=0, **stuck))[0] == "NOT_PRODUCTION"
    assert _verdict(_body(a0=1, a1=0, **stuck))[0] == "NOT_PRODUCTION"
    assert _verdict(_body(a0=0, a1=1, **stuck))[0] == "NOT_PRODUCTION"


def test_a_counter_that_went_backwards_is_a_new_session_not_a_stall():
    assert _verdict(_body(frames=(5000, 10), bytes_=(9000, 20)))[0] == "OK"


def test_unread_facts_are_unknown_never_a_page():
    p = sod.parse_probe(_body())
    for key in ("scene", "active0", "active1", "render0", "frames1", "bytes0"):
        q = dict(p)
        q[key] = ""
        assert sod.classify(q, 1, PRO)[0] == "UNKNOWN", key
    assert sod.classify(sod.parse_probe(""), 1, PRO)[0] == "UNKNOWN"
    assert sod.classify(sod.parse_probe(_body()), 1, "")[0] == "UNKNOWN"


def test_an_unreachable_box_skips():
    assert sod.classify(sod.parse_probe(_body(render=(1, 1))), 0, PRO) == ("SKIP", [])
    assert sod.analyze("", 0, PRO)["verdict"] == "SKIP"


def test_cli_prints_verdict_reasons_and_deltas():
    r = subprocess.run([sys.executable, str(_DECIDE), "analyze", "--box-reachable", "1",
                        "--production-scene", PRO], input=_body(bytes_=(7, 7)),
                       capture_output=True, text=True, check=True)
    assert "verdict=STUCK" in r.stdout and "reasons=bytes" in r.stdout, r.stdout
    assert "render_delta=90" in r.stdout and "bytes_delta=0" in r.stdout, r.stdout


def test_cli_requires_the_production_scene_argument():
    r = subprocess.run([sys.executable, str(_DECIDE), "analyze", "--box-reachable", "1"],
                       input=_body(), capture_output=True, text=True)
    assert r.returncode != 0


# --- the probe's printed lines (no network) -------------------------------------------------------

def test_probe_lines_round_trip_through_the_decision():
    try:
        probe = _load("stream_output_stuck_probe_1460", _PROBE)
    except SystemExit:  # websocket-client missing on this runner: the format is still pinned below
        return
    s0 = {"active": "1", "render": "10", "frames": "20", "bytes": "30", "reconnecting": "0"}
    s1 = {"active": "1", "render": "10", "frames": "25", "bytes": "40", "reconnecting": "0"}
    text = "\n".join(probe.format_lines(PRO, s0, s1, 3.0)) + "\n"
    assert _verdict(text) == ("STUCK", ["render"])
    s_unread = {"active": "", "render": "", "frames": "", "bytes": "", "reconnecting": ""}
    text = "\n".join(probe.format_lines(PRO, s_unread, s1, 3.0)) + "\n"
    assert _verdict(text)[0] == "UNKNOWN"


def test_probe_sends_only_read_requests():
    text = _PROBE.read_text()
    for banned in ("SetCurrentProgramScene", "SetCurrentPreviewScene", "StartStream", "StopStream",
                   "StartRecord", "StopRecord", "SetInputSettings"):
        assert banned not in text, banned
    assert "obs_phase2._rpc" in text and "obs_phase2._conn" in text


# --- the watchdog ---------------------------------------------------------------------------------

def _exe(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def _passes(tmp_path, bodies, rc=0):
    fetch_dir = tmp_path / "fetch"
    fetch_dir.mkdir(exist_ok=True)
    outs = []
    env = dict(os.environ)
    env.update({
        "STREAM_OUTPUT_STUCK_ALERT_STATE_DIR": str(tmp_path),
        "STREAM_OUTPUT_STUCK_WS_HOST": "192.0.2.1",
        "AIRULESET_NOTIFY": str(tmp_path / "no-notify.py"),
    })
    for i, body in enumerate(bodies):
        data = fetch_dir / f"body{i}.txt"
        data.write_text(body)
        fetch = _exe(fetch_dir / f"fetch{i}.sh", f"#!/usr/bin/env bash\ncat '{data}'\nexit {rc}\n")
        env["STREAM_OUTPUT_STUCK_FETCH_CMD"] = str(fetch)
        r = subprocess.run(["bash", str(_WATCHDOG), "--dry-run"], capture_output=True, text=True,
                           env=env, timeout=60)
        assert r.returncode == 0, r.stderr
        outs.append(r.stdout + r.stderr)
    return outs


def test_a_stuck_production_output_pages_after_two_passes(tmp_path):
    stuck = _body(frames=(50, 50), bytes_=(9, 9))
    first, second = _passes(tmp_path, [stuck, stuck])
    assert "WOULD page" not in first, first
    assert "WOULD page" in second, second
    assert "page:stream-snv-output-stuck" in second, second
    assert "encode,bytes" in second, second


def test_a_stuck_output_outside_production_never_pages(tmp_path):
    stuck = dict(render=(1, 1), frames=(2, 2), bytes_=(3, 3))
    outs = _passes(tmp_path, [_body(scene="Development", **stuck)] * 3)
    assert not any("WOULD page" in o for o in outs), outs
    outs = _passes(tmp_path, [_body(a0=0, a1=0, **stuck)] * 3)
    assert not any("WOULD page" in o for o in outs), outs


def test_a_healthy_pass_between_two_stuck_ones_resets_the_confirm(tmp_path):
    stuck = _body(bytes_=(9, 9))
    outs = _passes(tmp_path, [stuck, _body(), stuck])
    assert not any("WOULD page" in o for o in outs), outs


def test_an_unreachable_box_never_pages(tmp_path):
    outs = _passes(tmp_path, ["", ""], rc=1)
    assert not any("WOULD page" in o for o in outs), outs
    assert "SKIP" in outs[-1], outs[-1]


def test_recovery_after_a_page_is_a_log_line_only(tmp_path):
    stuck = _body(render=(5, 5))
    outs = _passes(tmp_path, [stuck, stuck, _body()])
    assert "WOULD page" in outs[1], outs[1]
    assert "WOULD send recovery" in outs[2] and "WOULD page" not in outs[2], outs[2]


def test_the_page_notify_line_carries_the_page_class_and_the_bucket_helper():
    lines = [ln for ln in _WATCHDOG.read_text().splitlines()
             if "page:${BOX}-output-stuck:" in ln and "dedup-key" in ln]
    assert len(lines) == 1, lines
    assert "watchdog_notify_key" in lines[0], lines[0]


def test_the_watchdog_never_retypes_the_production_scene():
    text = _WATCHDOG.read_text()
    assert 'BOX="${STREAM_OUTPUT_STUCK_BOX:-stream-snv}"' in text
    assert "STREAM_PRODUCTION_SCENE_DEFAULT" in text
    assert '"PRO"' not in text and "'PRO'" not in text and "=PRO" not in text


def test_the_unit_ships_disabled_and_spreads_its_start():
    timer = (_UNITS / "stream-output-stuck-alert-watchdog.timer").read_text()
    service = (_UNITS / "stream-output-stuck-alert-watchdog.service").read_text()
    readme = (_UNITS / "stream-output-stuck-alert-watchdog.README.md").read_text()
    assert "OnUnitActiveSec=1min" in timer and "RandomizedDelaySec=" in timer
    assert "stream-output-stuck-alert-watchdog.sh" in service
    assert "DISABLED" in readme and "systemctl --user enable --now" in readme
