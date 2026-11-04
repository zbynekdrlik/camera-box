"""issue 1387 (4.11.2026 20:49, strih-snv reboot): the strih OBS started 4 s before the rig NIC had a
carrier, the JPEG XS program output failed once with ENETUNREACH (`did not start`) and never retried,
and stream-snv's program stayed black ~30 min while no check noticed.

Pinned here:
  * strih-obs-start.sh waits (bounded, fail-open, logged) for the rig address the box-facts drop-in
    hands it (STRIH_RIG_IP) before it launches OBS;
  * verify-strih item 49's `(jxs-output-start)` row FAILs an enabled program output whose newest start
    line in the OBS log is `did not start` with no later `started --`;
  * item 49's lost-slices row FAILs a RUNNING JPEG XS source whose newest window carries frames=0 (the
    stream's `JXS PGM src` while the strih output is dead read PASS before: lost=0);
  * the strih-snv deploy read-back carries a `jxs=` field and refuses a failed / not yet started output;
  * the handover `jxs` item reads the stream side receiving (a stream `JXS PGM src` at frames=0 = FORGOT).

Every bash call runs under the callers' `set -euo pipefail`; values are passed as ARGUMENTS.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
HERE = Path(__file__).resolve().parent
START = SCRIPTS / "strih-obs-start.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
JXS_PROV = SCRIPTS / "lib" / "strih-jxs-provision.sh"
JXS_VERIFY = SCRIPTS / "lib" / "strih-jxs-verify.sh"
DEPLOY = SCRIPTS / "lib" / "strih-snv-deploy.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
OUTPUT = "camera-box JPEG XS program"
for p in (str(HERE), str(SCRIPTS)):
    if p not in sys.path:
        sys.path.insert(0, p)

import test_strih_jxs_verify_1387 as tv  # noqa: E402  (the window lines from the SHIPPED C format)


def _run(script, *args, env=None, cwd=None):
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + script, "harness", *map(str, args)],
                          capture_output=True, text=True, cwd=str(cwd or REPO), env=e, timeout=60)


# --- the launcher waits for the rig address ------------------------------------------------------------

def _fake_tools(tmp_path, appear_after):
    """`ip` prints the rig address from its Nth call on (0 = at once, -1 = never); `sleep` only logs."""
    bin_ = tmp_path / "bin"
    bin_.mkdir()
    (tmp_path / "calls").write_text("0")
    (bin_ / "ip").write_text(
        "#!/bin/bash\n"
        f'c=$(cat "{tmp_path}/calls"); echo $((c + 1)) > "{tmp_path}/calls"\n'
        'echo "1: lo    inet 127.0.0.1/8 scope host lo"\n'
        f'if [ {appear_after} -ge 0 ] && [ "$c" -ge {appear_after} ]; then\n'
        '  echo "2: enp7s0    inet 10.77.9.202/23 brd 10.77.9.255 scope global noprefixroute enp7s0"\n'
        'fi\n')
    (bin_ / "sleep").write_text(f'#!/bin/bash\necho "$*" >> "{tmp_path}/slept"\n')
    for f in ("ip", "sleep"):
        (bin_ / f).chmod(0o755)
    return {"PATH": f"{bin_}:{os.environ['PATH']}"}


def _wait(tmp_path, ip, bound, appear_after):
    return _run(f'. "{START}"\nset +e\nstrih_wait_rig_address "$1" "$2"\necho "RC=$?"', ip, bound,
                env=_fake_tools(tmp_path, appear_after))


def test_an_address_already_up_returns_at_once(tmp_path):
    r = _wait(tmp_path, "10.77.9.202", 120, 0)
    assert r.stdout.splitlines()[-1:] == ["RC=0"], r.stdout + r.stderr
    assert "10.77.9.202" in r.stdout and "up" in r.stdout
    assert not (tmp_path / "slept").exists()


def test_an_address_that_comes_later_is_waited_for(tmp_path):
    r = _wait(tmp_path, "10.77.9.202", 120, 4)
    assert r.stdout.splitlines()[-1:] == ["RC=0"], r.stdout + r.stderr
    assert (tmp_path / "slept").read_text().splitlines() == ["1"] * 4
    assert "after 4 s" in r.stdout, r.stdout


def test_an_address_that_never_comes_gives_up_at_the_bound(tmp_path):
    r = _wait(tmp_path, "10.77.9.202", 5, -1)
    assert r.stdout.splitlines()[-1:] == ["RC=1"], r.stdout + r.stderr
    assert len((tmp_path / "slept").read_text().splitlines()) == 5


def test_a_prefix_of_another_address_is_not_the_rig_address(tmp_path):
    r = _wait(tmp_path, "10.77.9.20", 2, 0)  # " 10.77.9.20/" never matches " 10.77.9.202/"
    assert r.stdout.splitlines()[-1:] == ["RC=1"], r.stdout


def test_no_or_a_malformed_address_is_not_waited_for(tmp_path):
    for ip in ("", "10.77.9", "10.77.9.202; reboot", "a.b.c.d"):
        r = _wait(tmp_path, ip, 120, -1)
        assert r.stdout.splitlines()[-1:] == ["RC=2"], (ip, r.stdout)
    assert not (tmp_path / "slept").exists()


def test_the_launcher_waits_before_it_launches_obs_and_never_stops_on_a_timeout():
    s = START.read_text()
    guard = s.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]; then')
    assert s.index("strih_wait_rig_address() {") < guard
    call = s.index('strih_wait_rig_address "${STRIH_RIG_IP:-}"')
    assert guard < call < s.index('"$OBS_BIN" "${OBS_ARGS[@]}" &')
    assert call < s.index('taskset -c "$ISOLATED_CPUS" "$OBS_BIN"')
    block = s[call:s.index("\n\n", call)]
    assert "WARN issue 1387" in block and "exit" not in block, block


def test_the_drop_in_hands_the_rig_address_to_the_launcher():
    r = _run(f'. "{PROVISION}"\nstrih_obs_box_facts_dropin_text')
    assert r.returncode == 0, r.stderr
    assert 'Environment="STRIH_RIG_IP=10.77.9.202"\n' in r.stdout, r.stdout


# --- verify item 49: the program output's start ----------------------------------------------------------

def _home(tmp_path, enabled=True):
    home = tmp_path / "home"
    f = home / ".config/obs-studio/plugin_config/distroav/jxs-output.json"
    f.parent.mkdir(parents=True)
    f.write_text(json.dumps({"enabled": enabled, "jxs_target": "10.77.9.204:7100", "jxs_stream_id": 100}))
    return home


LINE_CREATED = (f"20:49:02.966: [distroav] jxs-output: '{OUTPUT}' created from /x/jxs-output.json (read; "
                "enabled=true, target '10.77.9.204:7100') -- obs-websocket StartOutput (issue 1387)")
LINE_FAILED = (f"20:49:02.966: [distroav] jxs-output: '{OUTPUT}' did not start: 10.77.9.204:7100: Network is "
               "unreachable (os error 101) (issue 1387)")
LINE_STARTED = (f"21:20:40.940: [distroav] jxs-output '{OUTPUT}': started -- the program 1920x1080 NV12 at "
                "30.00 fps to 10.77.9.204:7100 (stream id 100, bpp 0..0) (issue 1387)")


def _start_row(tmp_path, lines, box="strih-snv", enabled=True):
    home = _home(tmp_path, enabled)
    log = tmp_path / "obs.txt"
    log.write_text("\n".join(lines) + "\n")
    body = (f'. "{PROVISION}"\nok() {{ echo "PASS $1"; }}; bad() {{ echo "FAIL $1"; }}; note() {{ echo "NOTE $1"; }}\n'
            'strih_box_load "$1"\nstrih_jxs_output_start_row "$2" "$3"')
    r = _run(body, box, home, log)
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def test_an_output_that_did_not_start_fails_naming_the_reason(tmp_path):
    rows = _start_row(tmp_path, [LINE_CREATED, LINE_FAILED, "20:55:00.000: later"])
    assert len(rows) == 1 and rows[0].startswith("FAIL (jxs-output-start)"), rows
    assert "Network is unreachable" in rows[0] and "20:49:02.966" in rows[0] and "StartOutput" in rows[0]


def test_a_later_start_clears_the_failure(tmp_path):
    rows = _start_row(tmp_path, [LINE_CREATED, LINE_FAILED, LINE_STARTED])
    assert rows == [f"PASS (jxs-output-start) '{OUTPUT}' started at 21:20:40.940 (the newest start line of the OBS log)"]


def test_a_failure_after_a_start_fails_again(tmp_path):
    later = LINE_FAILED.replace("20:49:02.966", "22:00:00.000")
    assert _start_row(tmp_path, [LINE_STARTED, later])[0].startswith("FAIL (jxs-output-start)")


def test_no_start_line_or_no_log_is_a_note(tmp_path):
    rows = _start_row(tmp_path, [LINE_CREATED])
    assert len(rows) == 1 and rows[0].startswith("NOTE (jxs-output-start)"), rows
    home = _home(tmp_path / "x")
    r = _run(f'. "{PROVISION}"\nnote() {{ echo "NOTE $1"; }}; ok() {{ :; }}; bad() {{ :; }}\n'
             'strih_box_load strih-snv\nstrih_jxs_output_start_row "$1" ""', home)
    assert r.stdout.startswith("NOTE (jxs-output-start)"), r.stdout + r.stderr


def test_a_disabled_output_or_the_stream_role_is_not_graded_here(tmp_path):
    assert _start_row(tmp_path / "a", [LINE_FAILED], enabled=False) == []
    assert _start_row(tmp_path / "b", [LINE_FAILED], box="stream-snv") == []


def test_verify_strih_runs_the_start_row_on_the_item_49_line_within_budget():
    v = VERIFY.read_text()
    assert len(v.splitlines()) <= 1000
    i = v.index('strih_jxs_config_verify_row "$USER_HOME"')
    line = v[v.rindex("\n", 0, i):v.index("\n", i)]
    assert 'strih_jxs_output_start_row "$USER_HOME" "$LOG"' in line, line


# --- verify item 49: a running source that receives nothing ----------------------------------------------

def _frames(line, frames):
    return line.replace(" frames=150 ", f" frames={frames} ").replace("fps=30.00", "fps=0.00" if not frames else "fps=30.00")


def test_a_running_source_with_no_frames_fails():
    zero = [_frames(tv.window(tv.ts(40_000 + 5 * k), "JXS PGM src", 0), 0) for k in range(4)]
    rows = tv._row(zero)
    assert len(rows) == 1 and rows[0].startswith("FAIL (jxs) 'JXS PGM src': frames=0"), rows
    assert "(issue 1387)" in rows[0]


def test_frames_flowing_again_passes():
    lines = [_frames(tv.window(tv.ts(40_000), "JXS PGM src", 0), 0), tv.window(tv.ts(40_005), "JXS PGM src", 0)]
    assert tv._row(lines) == ["PASS (jxs) 'JXS PGM src': lost_slices=0 in its last 2 window(s)"]


# --- the deploy read-back --------------------------------------------------------------------------------

def _verdict(*args):
    r = _run(f'. "{DEPLOY}"\nset +e\nstrih_lx_deploy_verdict "$@"\necho "RC=$?"', *args)
    return r.stdout


BASE = ["abc123", "abc123", "active", "abc123", "lib1", "lib1", "1", "v9.9.5", "v9.9.5", "", ""]


def test_the_read_back_passes_an_off_or_started_output():
    for jxs in ("off", "started"):
        out = _verdict(*BASE, jxs)
        assert "OK" in out and out.splitlines()[-1:] == ["RC=0"], (jxs, out)


def test_the_read_back_refuses_a_dead_or_not_yet_started_output():
    for jxs, word in (("failed", "did not start"), ("pending", "not started yet"), ("", "unreadable")):
        out = _verdict(*BASE, jxs)
        assert out.splitlines()[-1:] == ["RC=1"] and "JPEG XS" in out and word in out, (jxs, out)


def test_an_older_caller_without_the_field_is_not_graded_on_it():
    out = _verdict(*BASE[:7])
    assert out.splitlines()[-1:] == ["RC=0"], out


def _jxs_state(tmp_path, log_lines, enabled=True, fresh=True):
    home = _home(tmp_path, enabled)
    logs = home / ".config/obs-studio/logs"
    logs.mkdir(parents=True)
    marker = tmp_path / "obs-start.marker"
    marker.write_text("")
    log = logs / "2026-11-04 20-49-01.txt"
    log.write_text("\n".join(log_lines) + "\n")
    t = os.stat(marker).st_mtime
    os.utime(log, (t + (10 if fresh else -10), t + (10 if fresh else -10)))
    r = _run(f'. "{DEPLOY}"\nL="$1"; M="$2"\neval "$(strih_lx_remote_jxs_state_cmd)"\necho "J=$J"', log, marker,
             env={"HOME": str(home)})
    assert r.returncode == 0, r.stderr
    return r.stdout.strip().splitlines()[-1]


def test_the_on_box_state_reads_the_log_after_the_start(tmp_path):
    assert _jxs_state(tmp_path / "a", [LINE_CREATED, LINE_FAILED]) == "J=failed"
    assert _jxs_state(tmp_path / "b", [LINE_FAILED, LINE_STARTED]) == "J=started"
    assert _jxs_state(tmp_path / "c", [LINE_CREATED]) == "J=pending"
    assert _jxs_state(tmp_path / "d", [LINE_STARTED], fresh=False) == "J=pending"
    assert _jxs_state(tmp_path / "e", [LINE_FAILED], enabled=False) == "J=off"


def test_the_read_back_line_carries_the_field_and_the_verify_poll_grades_it():
    s = DEPLOY.read_text()
    r = _run(f'. "{DEPLOY}"\nstrih_lx_remote_readback_cmd /tmp/genlock-stage-x')
    assert " jxs=%s" in r.stdout and "jxs-output.json" in r.stdout, r.stdout
    assert '"$(_strih_lx_field jxs "$line")"' in s


# --- the handover check reads the stream side ------------------------------------------------------------

def test_the_handover_item_forgets_a_stream_program_input_that_receives_nothing():
    import test_rig_dev_handover_jxs_1387 as th
    live = json.loads(th.LIVE_OK)
    live["stream"]["lines"] = [th._src("13:30:05.000", "JXS PGM src", 0)]
    e = th._decide(th.STRIH_ROWS, th.STREAM_ROWS, json.dumps(live))
    assert e["status"] == th.d.FORGOT, e
    assert "stream: 'JXS PGM src' 0 snimok" in e["message"], e
