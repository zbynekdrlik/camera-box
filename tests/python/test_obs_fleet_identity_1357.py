"""issue 1357 (follow-up 5866910703) -- `obs_fleet_poll_now` checks WHICH machine answers.

Before: a `traveling` fleet box was polled whenever its row host resolved and OBS-WS :4455 answered.
`resolume.lan` resolves to 10.77.9.201, an address the `bridge` PC has also held, so whichever PC
answered there had the win-baseline gather program copied onto it and was graded as resolume-snv;
obs-session and obs-liveness would read it the same way.

Now: the expected computer name of each DHCP-named row is declared ONCE in scripts/lib/obs-fleet.sh
(`OBS_FLEET_IDENTITY`), and `obs_fleet_poll_now` reads the name that answers before any per-box read
or write. A different name, or no readable name, is rc 2 = UNKNOWN naming the machine that answered,
and nothing is copied. Every reader gets it; win-baseline-check, obs-session and obs-liveness name it.

Offline: the identity seam `OBS_FLEET_IDENTITY_CMD`, `OBS_FLEET_HOME`, a fake `sshpass` for the
default reader, the win-baseline `WIN_BASELINE_FETCH_CMD` seam and the obs-liveness echo probe.
"""
import os
import pathlib
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FLEET = SCRIPTS / "lib" / "obs-fleet.sh"
WIN_CHECK = SCRIPTS / "win-baseline-check.sh"
SESSION = SCRIPTS / "obs-session-watchdog.sh"
LIVENESS = SCRIPTS / "obs-liveness-watchdog.sh"
ECHO_PROBE = REPO / "tests" / "fixtures" / "obs_liveness_echo_probe_1296.py"
FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "win_baseline_1357"

STRIP = ("OBS_FLEET", "OBS_FLEET_HOME", "OBS_FLEET_IDENTITY", "OBS_FLEET_IDENTITY_CMD",
         "OBS_SESSION_WATCHDOG_BOXES", "STRIH_SNV_HOST", "STREAM_SNV_HOST", "RESOLUME_SNV_HOST",
         "WIN_BASELINE_FETCH_CMD")


def _env(**extra):
    env = {k: v for k, v in os.environ.items() if k not in STRIP}
    env.update(extra)
    return env


def _bash(script, *args, strict="set -euo pipefail", **extra):
    """Run SCRIPT under the callers' strict mode (the lib's consumers run `set -euo pipefail`, the
    watchdogs `set -uo pipefail`); values reach it as ARGUMENTS only."""
    return subprocess.run(["bash", "-c", strict + "\n" + script, "harness", *args],
                          capture_output=True, text=True, env=_env(**extra), timeout=60)


def _identity_seam(tmp_path, answer, rc=0):
    """An OBS_FLEET_IDENTITY_CMD stand-in: logs `<name> <host>`, prints ANSWER, exits RC."""
    log = tmp_path / "identity.log"
    seam = tmp_path / "identity.sh"
    seam.write_text('#!/usr/bin/env bash\n'
                    f'printf "%s %s\\n" "$1" "$2" >>"{log}"\n'
                    f'printf "%s\\r\\n" "{answer}"\n'
                    f'exit {rc}\n')
    seam.chmod(0o755)
    return str(seam), log


def _poll(name, **extra):
    return _bash('. "$1"; rc=0; obs_fleet_poll_now "$2" || rc=$?; '
                 'echo "RC=$rc"; echo "REASON=${OBS_FLEET_IDENTITY_REASON:-}"', str(FLEET), name,
                 **extra)


# --- the declaration ------------------------------------------------------------------------------

def test_expected_names_are_declared_once_in_the_fleet_lib():
    r = _bash('. "$1"; for n in resolume-snv strih-pp stream-pp; do '
              'printf "%s=%s\\n" "$n" "$(obs_fleet_expected_identity "$n")"; done; '
              'obs_fleet_expected_identity strih-snv || echo "strih-snv=NONE"', str(FLEET))
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["resolume-snv=RESOLUME-SNV", "strih-pp=strih-pp",
                                "stream-pp=stream-pp", "strih-snv=NONE"]
    # The readers never hand-type an expected name.
    for script in (WIN_CHECK, SESSION, LIVENESS):
        assert "OBS_FLEET_IDENTITY=" not in script.read_text(), script


def test_an_old_alias_finds_the_renamed_rows_expected_name():
    r = _bash('. "$1"; obs_fleet_expected_identity resolume', str(FLEET))
    assert r.returncode == 0 and r.stdout == "RESOLUME-SNV", (r.stdout, r.stderr)


# --- the gate in obs_fleet_poll_now ---------------------------------------------------------------

def test_the_right_name_polls_case_and_cr_tolerant(tmp_path):
    seam, log = _identity_seam(tmp_path, "resolume-snv")
    r = _poll("resolume-snv", OBS_FLEET_HOME="resolume-snv", OBS_FLEET_IDENTITY_CMD=seam)
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert log.read_text().split() == ["resolume-snv", "resolume.lan"]


def test_a_wrong_name_is_unknown_naming_the_machine_that_answered(tmp_path):
    seam, _ = _identity_seam(tmp_path, "BRIDGE")
    r = _poll("resolume-snv", OBS_FLEET_HOME="resolume-snv", OBS_FLEET_IDENTITY_CMD=seam)
    assert "RC=2" in r.stdout, r.stdout + r.stderr
    assert "BRIDGE" in r.stdout and "RESOLUME-SNV" in r.stdout, r.stdout
    assert "BRIDGE" in r.stderr, "one stderr line names it for every reader"


def test_an_unreadable_name_is_unknown(tmp_path):
    seam, _ = _identity_seam(tmp_path, "", rc=255)
    r = _poll("resolume-snv", OBS_FLEET_HOME="resolume-snv", OBS_FLEET_IDENTITY_CMD=seam)
    assert "RC=2" in r.stdout, r.stdout + r.stderr
    assert "unreadable" in r.stdout, r.stdout


def test_an_empty_answer_is_unreadable_even_with_rc_0(tmp_path):
    seam, _ = _identity_seam(tmp_path, "")
    r = _poll("resolume-snv", OBS_FLEET_HOME="resolume-snv", OBS_FLEET_IDENTITY_CMD=seam)
    assert "RC=2" in r.stdout and "unreadable" in r.stdout, r.stdout


def test_an_away_box_is_rc_1_and_never_read(tmp_path):
    seam, log = _identity_seam(tmp_path, "resolume-snv")
    r = _poll("resolume-snv", OBS_FLEET_HOME="nobody", OBS_FLEET_IDENTITY_CMD=seam)
    assert "RC=1" in r.stdout, r.stdout
    assert not log.exists()


def test_a_fixed_box_and_an_undeclared_name_are_never_read(tmp_path):
    seam, log = _identity_seam(tmp_path, "BRIDGE")
    for name in ("strih-snv", "stream-snv", "strih-zz"):
        r = _poll(name, OBS_FLEET_HOME="resolume-snv", OBS_FLEET_IDENTITY_CMD=seam)
        assert "RC=0" in r.stdout, (name, r.stdout, r.stderr)
    assert not log.exists()


def test_the_default_reader_asks_the_box_its_hostname_over_ssh(tmp_path):
    """No seam: the lib dials `hostname` through sshpass (a fake on PATH plays a Windows box)."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    calls = tmp_path / "ssh.log"
    fake = bindir / "sshpass"
    fake.write_text('#!/usr/bin/env bash\n'
                    f'printf "%s\\x1f" "$@" >>"{calls}"; echo >>"{calls}"\n'
                    'printf "RESOLUME-SNV\\r\\n"\n')
    fake.chmod(0o755)
    seam_free = {"OBS_FLEET_HOME": "", "PATH": f"{bindir}:{os.environ['PATH']}"}
    r = _bash('. "$1"; obs_fleet_status_probe() { printf 1; }; '
              'obs_fleet_resolve_host() { printf 10.77.9.201; }; '
              'rc=0; obs_fleet_poll_now resolume-snv || rc=$?; echo "RC=$rc"', str(FLEET),
              **seam_free)
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    args = calls.read_text().split("\x1f")
    assert "newlevel@resolume.lan" in args and args[-2] == "hostname", args


# --- win-baseline-check.sh ------------------------------------------------------------------------

def _fetch_seam(tmp_path):
    log = tmp_path / "fetch.log"
    seam = tmp_path / "fetch.sh"
    seam.write_text('#!/usr/bin/env bash\n'
                    f'echo "$1 $2" >>"{log}"\n'
                    f'cp "{FIX / "resolume_live_2026-09-27.txt"}" "$3"\n')
    seam.chmod(0o755)
    return str(seam), log


def _win_check(tmp_path, identity_seam):
    fetch, log = _fetch_seam(tmp_path)
    r = subprocess.run(["bash", str(WIN_CHECK), "--out-dir", str(tmp_path / "out")],
                       capture_output=True, text=True, timeout=60,
                       env=_env(WIN_BASELINE_FETCH_CMD=fetch, OBS_FLEET_HOME="resolume-snv",
                                OBS_FLEET_IDENTITY_CMD=identity_seam))
    return r, log


def test_win_baseline_grades_the_right_box(tmp_path):
    seam, _ = _identity_seam(tmp_path, "RESOLUME-SNV")
    r, log = _win_check(tmp_path, seam)
    assert log.read_text().split() == ["resolume-snv", "resolume.lan"], r.stdout + r.stderr
    assert "box=resolume-snv win_baseline=UNKNOWN" not in r.stdout, r.stdout


def test_win_baseline_wrong_machine_is_unknown_and_copies_nothing(tmp_path):
    seam, _ = _identity_seam(tmp_path, "BRIDGE")
    r, log = _win_check(tmp_path, seam)
    assert r.returncode == 11, r.stdout + r.stderr
    line = [ln for ln in r.stdout.splitlines() if ln.startswith("box=resolume-snv win_baseline=")]
    assert line and "UNKNOWN" in line[0] and "BRIDGE" in line[0], r.stdout
    assert not log.exists(), "the gather must never be copied to the wrong machine"
    assert not (tmp_path / "out" / "resolume-snv.txt").exists()


def test_win_baseline_unreadable_name_is_unknown(tmp_path):
    seam, _ = _identity_seam(tmp_path, "", rc=255)
    r, log = _win_check(tmp_path, seam)
    assert r.returncode == 11, r.stdout + r.stderr
    assert "win_baseline=UNKNOWN" in r.stdout and "unreadable" in r.stdout, r.stdout
    assert not log.exists()


# --- obs-session + obs-liveness -------------------------------------------------------------------

def test_obs_session_never_probes_the_wrong_machine(tmp_path):
    seam, _ = _identity_seam(tmp_path, "BRIDGE")
    r = _bash('wd="$1"; set --; . "$wd"; obs_session_targets', str(SESSION), strict="set -uo pipefail",
              OBS_FLEET_HOME="resolume-snv", OBS_FLEET_IDENTITY_CMD=seam)
    assert r.returncode == 0, r.stderr
    names = [ln.split()[0] for ln in r.stdout.splitlines() if ln.strip()]
    assert names == ["strih-snv", "stream-snv"], r.stdout
    assert "resolume-snv" in r.stderr and "BRIDGE" in r.stderr and "UNKNOWN" in r.stderr, r.stderr


def test_obs_session_probes_the_right_machine(tmp_path):
    seam, _ = _identity_seam(tmp_path, "resolume-snv")
    r = _bash('wd="$1"; set --; . "$wd"; obs_session_targets', str(SESSION), strict="set -uo pipefail",
              OBS_FLEET_HOME="resolume-snv", OBS_FLEET_IDENTITY_CMD=seam)
    assert r.returncode == 0, r.stderr
    assert "resolume-snv resolume.lan guard" in r.stdout, r.stdout


def _liveness(tmp_path, answer):
    seam, _ = _identity_seam(tmp_path, answer)
    return _bash('wd="$1"; set --; . "$wd"; measure_boxes; printf "%s" "$VERDICT_LINES"', str(LIVENESS),
                 strict="set -uo pipefail",
                 OBS_FLEET_HOME="resolume-snv", OBS_FLEET_IDENTITY_CMD=seam,
                 OBS_LIVENESS_PROBE=str(ECHO_PROBE))


def test_obs_liveness_never_polls_the_wrong_machine(tmp_path):
    r = _liveness(tmp_path, "BRIDGE")
    assert r.returncode == 0, r.stderr
    assert "strih-snv" in r.stdout and "resolume-snv" not in r.stdout, r.stdout
    assert "BRIDGE" in r.stderr, r.stderr


def test_obs_liveness_polls_the_right_machine(tmp_path):
    r = _liveness(tmp_path, "RESOLUME-SNV")
    assert r.returncode == 0, r.stderr
    assert "box=resolume-snv=resolume.lan:60" in r.stdout, r.stdout
