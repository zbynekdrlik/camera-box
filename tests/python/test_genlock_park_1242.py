"""issue 1242 — every consumer of the strih `genlock-fifo received=` tap must read a HIDDEN-BY-DESIGN
(parked) program-path camera input as SKIP, never as FROZEN / wrong-cadence / halved / arrivals-low.

The vendored DistroAV receiver parks a genlocked `genlock_connect_on_show` input while nothing shows
it and logs `genlock-park '<src>': state=parked ...` (a 5 s heartbeat) / `state=unparked` on show.
The always-connected low-bandwidth `MV NDI camN` twin keeps the camera leg observable.

Covers:
  * the ONE python parser (`scripts/genlock_park.py`) + its bash twin (`scripts/lib/genlock-park.sh`),
    pinned to each other over the same fixtures;
  * the strih frozen-input watchdog (#1069 enumeration mode: read the twin, never both; static mode:
    a parked source is SKIP);
  * the cadence watchdog (a parked source is SKIP, never a blind-tap WARN);
  * the ndi-halving watchdog (a parked input is SKIP);
  * rig-health-audit's arrivals-low term (parked inputs + monitor twins are not program ingest).
"""
import importlib.util
import os
import pathlib
import subprocess
import textwrap

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
BASH_LIB = SCRIPTS / "lib" / "genlock-park.sh"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _park():
    return _load("genlock_park_1242", SCRIPTS / "genlock_park.py")


PARKED_LINE = ("genlock-park '{src}': state=parked parked_s={s} "
               "(connect-on-show, hidden; NDI receiver released, issue 1242)")
UNPARKED_LINE = "genlock-park '{src}': state=unparked parked_s={s} (shown; reconnecting, issue 1242)"


def _audit(src, received, ts="12:00:00.000"):
    return f"{ts}: genlock-fifo audit '{src}': received={received} consumed=1 locked=1"


FIXTURE = "\n".join([
    _audit("NDI cam1", 100),
    _audit("MV NDI cam1", 100),
    "12:00:00.100: " + PARKED_LINE.format(src="NDI cam2", s=0),
    _audit("MV NDI cam2", 90),
    "12:00:01.000: " + PARKED_LINE.format(src="NDI cam3", s=40),
    "12:00:02.000: " + UNPARKED_LINE.format(src="NDI cam3", s=41),
    _audit("NDI cam3", 55),
    _audit("MV NDI cam3", 55),
    _audit("MV NDI cam4", 70),
    "12:00:03.000: " + PARKED_LINE.format(src="NDI cam5", s=12),
]) + "\n"


# ------------------------------------------------------------------------------------------------
# the python parser
# ------------------------------------------------------------------------------------------------

def test_state_of_reads_the_last_park_line_per_source():
    gp = _park()
    assert gp.park_state_of(FIXTURE, "NDI cam2") == "parked"
    assert gp.park_state_of(FIXTURE, "NDI cam3") == "unparked"  # the LAST line wins
    assert gp.park_state_of(FIXTURE, "NDI cam1") is None       # never parked in the window
    # quote-anchored: the twin's name never matches its main's line and vice versa
    assert gp.park_state_of(FIXTURE, "MV NDI cam2") is None


def test_parked_sources_is_the_set_whose_last_state_is_parked():
    gp = _park()
    assert gp.parked_sources(FIXTURE) == {"NDI cam2", "NDI cam5"}
    assert gp.parked_sources("") == set()


def test_monitor_twin_naming():
    gp = _park()
    assert gp.is_monitor_twin("MV NDI cam3")
    assert not gp.is_monitor_twin("NDI cam3")
    assert gp.twin_of("NDI cam3") == "MV NDI cam3"
    assert gp.main_of("MV NDI cam3") == "NDI cam3"


def test_watch_set_reads_exactly_one_live_receiver_per_camera():
    gp = _park()
    names = ["NDI cam1", "MV NDI cam1", "MV NDI cam2", "NDI cam2", "NDI cam3", "MV NDI cam3",
             "MV NDI cam4", "NDI cam5"]
    # cam1: main live -> main only; cam2: main parked -> the twin; cam3: unparked -> main;
    # cam4: only a twin enumerated -> the twin; cam5: parked with no twin -> nothing (hidden by design).
    assert gp.watch_set(names, FIXTURE) == ["NDI cam1", "MV NDI cam2", "NDI cam3", "MV NDI cam4"]


def test_log_bytes_are_safe():
    # A non-UTF-8 byte elsewhere in the tail must never blind the parser (the issue-1258 class).
    gp = _park()
    raw = FIXTURE.encode() + b"12:00:04.000: junk \xff\xfe line\n"
    assert gp.parked_sources(raw.decode("utf-8", "replace")) == {"NDI cam2", "NDI cam5"}


# ------------------------------------------------------------------------------------------------
# the bash twin — pinned to the python parser over the same fixture
# ------------------------------------------------------------------------------------------------

def _bash(body, stdin=""):
    out = subprocess.run(
        ["bash", "-c", f"set -euo pipefail; . '{BASH_LIB}'; {body}"],
        input=stdin, capture_output=True, text=True, check=False,
        env={**os.environ, "LC_ALL": "C.UTF-8"},
    )
    return out


@pytest.mark.parametrize("src", ["NDI cam1", "NDI cam2", "NDI cam3", "MV NDI cam2", "NDI cam5"])
def test_bash_state_of_matches_python(src):
    gp = _park()
    out = _bash(f"genlock_park_state_of '{src}'", FIXTURE)
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == (gp.park_state_of(FIXTURE, src) or "")


def test_bash_watch_set_matches_python():
    gp = _park()
    names = ["NDI cam1", "MV NDI cam1", "MV NDI cam2", "NDI cam2", "NDI cam3", "MV NDI cam3",
             "MV NDI cam4", "NDI cam5"]
    joined = "\n".join(names)
    out = _bash(f"genlock_park_watch_set \"$(printf '%s' '{joined}')\"", FIXTURE)
    assert out.returncode == 0, out.stderr
    assert out.stdout.split("\n")[:-1] == gp.watch_set(names, FIXTURE)


def test_bash_helpers_survive_set_euo_pipefail_on_no_match():
    out = _bash("x=\"$(genlock_park_state_of 'NDI cam9')\"; echo \"[$x]\"", "")
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == "[]"


# ------------------------------------------------------------------------------------------------
# watchdogs (all I/O stubbed)
# ------------------------------------------------------------------------------------------------

def _stub(path, body):
    path.write_text(textwrap.dedent(body))
    path.chmod(0o755)
    return path


NOTIFY_STUB = """\
    #!/usr/bin/env python3
    import sys, os
    with open(os.environ["STUB_NOTIFY_LOG"], "a") as f:
        f.write("\\n---NOTIFY---\\n" + "\\n".join(sys.argv[1:]) + "\\n")
"""


def _frozen_rig(tmp_path, log_body, probe_body):
    state = tmp_path / "state"
    state.mkdir()
    notify_log = tmp_path / "notify.log"
    notify_log.write_text("")
    enum = _stub(tmp_path / "enum.sh", "#!/usr/bin/env bash\ncat <<'LOG'\n" + log_body + "LOG\n")
    probe = _stub(tmp_path / "probe.sh", probe_body)
    notify = _stub(tmp_path / "notify.py", NOTIFY_STUB)
    return state, notify_log, enum, probe, notify


# Each call prints the source's audit line: the MAIN 'NDI cam2' is stuck (parked -> no frames) and
# carries its park heartbeat; every other source (incl. every twin) advances per call.
PROBE_PARKED_CAM2 = """\
    #!/usr/bin/env bash
    src="$2"
    key=$(printf '%s' "$src" | tr -c 'A-Za-z0-9' '_')
    cf="$STUB_STATE/cnt_$key"
    n=$(cat "$cf" 2>/dev/null || echo 0); n=$((n+1)); printf '%s' "$n" > "$cf"
    if [ "$src" = "NDI cam2" ]; then
      printf "12:00:00.000: genlock-fifo audit '%s': received=5000 consumed=1 locked=1\\n" "$src"
      printf "12:00:00.500: genlock-park 'NDI cam2': state=parked parked_s=9 (connect-on-show, hidden; NDI receiver released, issue 1242)\\n"
    else
      printf "12:00:00.000: genlock-fifo audit '%s': received=%s consumed=1 locked=1\\n" "$src" "$((5000 + n))"
    fi
"""

ENUM_LOG = "\n".join([
    _audit("NDI cam1", 1),
    _audit("MV NDI cam1", 1),
    _audit("NDI cam2", 1),
    _audit("MV NDI cam2", 1),
    "12:00:00.500: " + PARKED_LINE.format(src="NDI cam2", s=9),
    _audit("NDI 2ME PGM (mv)", 1),
]) + "\n"


def _run_frozen(tmp_path, state, notify_log, enum, probe, notify, extra_env):
    env = {
        **os.environ,
        "FROZEN_INPUT_RECEIVER": "strih|10.77.9.202",
        "FROZEN_INPUT_SENDER": "strih",
        "FROZEN_INPUT_ALERT_TAG": "#1069",
        "FROZEN_INPUT_ENUMERATE_CMD": str(enum),
        "FROZEN_INPUT_PROBE_CMD": str(probe),
        "AIRULESET_NOTIFY": str(notify),
        "STUB_NOTIFY_LOG": str(notify_log),
        "STUB_STATE": str(state),
        "FROZEN_INPUT_ALERT_STATE_DIR": str(state),
        "FROZEN_INPUT_ALERT_STATE_FILE": str(state / "strih.state"),
        "FROZEN_INPUT_NETREACH_STATE_FILE": str(state / "absent.state"),
        "FROZEN_INPUT_ALERT_CONFIRM_THRESHOLD": "1",
        "FROZEN_INPUT_TAP_BROKEN_THRESHOLD": "1",
        **extra_env,
    }
    out = subprocess.run(["bash", str(SCRIPTS / "frozen-input-alert-watchdog.sh")],
                         env=env, capture_output=True, text=True, cwd=str(REPO), check=False)
    assert out.returncode == 0, out.stdout + out.stderr
    return out.stderr


def test_frozen_enum_mode_reads_the_twin_for_a_parked_main_and_never_pages_it(tmp_path):
    state, notify_log, enum, probe, notify = _frozen_rig(tmp_path, ENUM_LOG, PROBE_PARKED_CAM2)
    for _ in range(3):
        err = _run_frozen(tmp_path, state, notify_log, enum, probe, notify,
                          {"FROZEN_INPUT_ENUMERATE": "1"})
    assert notify_log.read_text().strip() == "", "a parked (hidden by design) input must never page"
    # the parked main is not watched; its always-connected twin is.
    assert "'MV NDI cam2'" in err
    assert "'NDI cam2' on strih" not in err
    # a live main is watched ONCE (its twin is not also watched -> no double page for one camera).
    assert "'NDI cam1' on strih" in err
    assert "'MV NDI cam1' on strih" not in err


def test_frozen_static_mode_skips_a_parked_source(tmp_path):
    state, notify_log, enum, probe, notify = _frozen_rig(tmp_path, ENUM_LOG, PROBE_PARKED_CAM2)
    for _ in range(3):
        err = _run_frozen(tmp_path, state, notify_log, enum, probe, notify,
                          {"FROZEN_INPUT_SOURCES": "NDI cam2"})
    assert notify_log.read_text().strip() == "", "a parked static source must never page (not even tap-broken)"
    assert "hidden by design" in err and "SKIP" in err


def _run_cadence(tmp_path, log_text):
    state = tmp_path / "cstate"
    state.mkdir(exist_ok=True)
    notify_log = tmp_path / "cnotify.log"
    notify_log.touch()
    probe = _stub(tmp_path / "cprobe.sh", "#!/usr/bin/env bash\ncat <<'LOG'\n" + log_text + "LOG\n")
    notify = _stub(tmp_path / "cnotify.py", NOTIFY_STUB)
    env = {
        **os.environ,
        "CADENCE_BOX": "strih|10.77.9.202",
        "CADENCE_SOURCES": "NDI cam2",
        "CADENCE_PROBE_CMD": str(probe),
        "AIRULESET_NOTIFY": str(notify),
        "STUB_NOTIFY_LOG": str(notify_log),
        "CADENCE_ALERT_STATE_DIR": str(state),
        "CADENCE_ALERT_STATE_FILE": str(state / "c.state"),
        "CADENCE_NETREACH_STATE_FILE": str(state / "absent.state"),
        "CADENCE_ALERT_CONFIRM_THRESHOLD": "1",
        "CADENCE_TAP_BROKEN_THRESHOLD": "1",
    }
    out = subprocess.run(["bash", str(SCRIPTS / "cadence-alert-watchdog.sh")], env=env,
                         capture_output=True, text=True, cwd=str(REPO), check=False)
    assert out.returncode == 0, out.stdout + out.stderr
    return out.stderr, notify_log.read_text()


def test_cadence_skips_a_parked_source_and_never_calls_its_tap_blind(tmp_path):
    parked_only = "12:00:00.000: " + PARKED_LINE.format(src="NDI cam2", s=30) + "\n"
    for _ in range(3):
        err, notified = _run_cadence(tmp_path, parked_only)
    assert notified.strip() == "", "a parked source is hidden by design -- never a tap-broken WARN"
    assert "hidden by design" in err and "SKIP" in err


def test_ndi_halving_skips_a_parked_input(tmp_path):
    state = tmp_path / "hstate"
    state.mkdir()
    notify_log = tmp_path / "hnotify.log"
    notify_log.touch()
    parked_only = "12:00:00.000: " + PARKED_LINE.format(src="NDI cam2", s=30) + "\n"
    probe = _stub(tmp_path / "hprobe.sh", "#!/usr/bin/env bash\ncat <<'LOG'\n" + parked_only + "LOG\n")
    notify = _stub(tmp_path / "hnotify.py", NOTIFY_STUB)
    env = {
        **os.environ,
        "NDI_HALVING_RECEIVER": "strih|10.77.9.202",
        "NDI_HALVING_INPUTS": "NDI cam2|60",
        "NDI_HALVING_PROBE_CMD": str(probe),
        "AIRULESET_NOTIFY": str(notify),
        "STUB_NOTIFY_LOG": str(notify_log),
        "NDI_HALVING_STATE_DIR": str(state),
        "NDI_HALVING_STATE_FILE": str(state / "h.state"),
        "NDI_HALVING_NETREACH_STATE_FILE": str(state / "absent.state"),
        "NDI_HALVING_TAP_BROKEN_THRESHOLD": "1",
    }
    for _ in range(2):
        out = subprocess.run(["bash", str(SCRIPTS / "ndi-halving-watchdog.sh")], env=env,
                             capture_output=True, text=True, cwd=str(REPO), check=False)
        assert out.returncode == 0, out.stdout + out.stderr
    assert notify_log.read_text().strip() == ""
    assert "hidden by design" in out.stderr and "SKIP" in out.stderr


# ------------------------------------------------------------------------------------------------
# rig-health-audit arrivals-low
# ------------------------------------------------------------------------------------------------

def test_park_touched_sources_counts_parked_and_unparked():
    gp = _park()
    assert gp.park_touched_sources(FIXTURE) == {"NDI cam2", "NDI cam3", "NDI cam5"}


def test_rig_health_arrivals_low_excludes_an_input_unparked_inside_the_window():
    rha = _load("rig_health_audit_1242b", SCRIPTS / "rig-health-audit.py")
    rates = {"NDI cam3": 4.0}  # its last two samples straddle the cold reconnect
    log = "12:00:00.000: " + UNPARKED_LINE.format(src="NDI cam3", s=40) + "\n"
    assert rha.low_arrival_sources(rates, log) == []


def test_rig_health_cadence_check_skips_a_park_touched_camera():
    rha = _load("rig_health_audit_1242c", SCRIPTS / "rig-health-audit.py")
    # a 30 fps first-to-last span on cam2 (would read as a WRONG cadence) -- but cam2 parked in the window
    lines = [
        _audit("NDI cam2", 0, ts="12:00:00.000"),
        "12:00:10.000: " + PARKED_LINE.format(src="NDI cam2", s=0),
        _audit("NDI cam2", 3000, ts="12:01:40.000"),
    ]
    display, problems = rha.cadence_check("\n".join(lines) + "\n")
    assert "cam2" not in display and problems == []
    # control: the SAME samples with no park line are graded (proves the skip, not a blind check)
    display, problems = rha.cadence_check("\n".join([lines[0], lines[2]]) + "\n")
    assert any("cam2" in p for p in problems)


def test_rig_health_arrivals_low_excludes_parked_inputs_and_monitor_twins():
    rha = _load("rig_health_audit_1242", SCRIPTS / "rig-health-audit.py")
    rates = {"NDI cam1": 60.0, "NDI cam2": 0.0, "MV NDI cam3": 25.0, "NDI cam4": 10.0}
    log = "12:00:00.000: " + PARKED_LINE.format(src="NDI cam2", s=5) + "\n"
    # cam2 is parked (hidden by design), MV NDI cam3 is a monitor twin (not program ingest);
    # cam4 is a real live input arriving too slowly -> the ONLY hard low.
    assert rha.low_arrival_sources(rates, log) == ["NDI cam4"]
