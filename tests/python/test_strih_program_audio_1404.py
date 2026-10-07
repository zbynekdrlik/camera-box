"""issue 1404 (ROZHODNUTÉ 6039368611) -- the stream program-audio sampler moves off dev1 to strih-lx.

Pinned here:
  * the strih-lx unit template: the E-cores through ONE `CPUAffinity=@E_CORES@` line rendered from the
    box's own /sys/devices/cpu_atom/cpus (the strih_lx_lowprio_prefix contract), normal priority, the
    opt-in TEST marker as an ExecCondition (down by default), the sampler from the installed
    checkout-layout copy;
  * setup-strih step 16e (`strih_program_audio_install`): numpy from apt only when missing, the files
    written only when they differ, the decoder shim built (as the operator) only when it is missing or
    was built from other sources, the unit written only when it differs, enable + daemon-reload +
    try-restart -- NEVER a start;
  * verify-strih item 41 (`strih_program_audio_grade_rows`): files + unit + enabled, the shim current,
    the unit's state against the TEST marker (running in TEST mode = a FRESH verdict -- the guard's
    -1 s .. --max-age window -- on the env file's endpoint, read with retries; down without the marker =
    a NOTE; running WITHOUT the marker, down with it, failed, a crash loop or an unreadable state = a
    FAIL);
  * rig-mode.sh: TEST leaves the marker, clears a failed state and starts the sampler (read 2 s later),
    EVENT removes the marker, stops it and clears a failed state (`scripts/lib/program-audio-mode.sh`),
    report-only;
  * the installed file list covers the sampler's whole import closure + the shim's sources.

Every bash run sources the lib under the caller's `set -euo pipefail`; values reach bash as
ARGUMENTS, never inside the script text. Fake dpkg-query / apt-get / systemctl / curl / sshpass on
PATH; the real python3, numpy and g++ (the shim really builds).
"""
from __future__ import annotations

import ast
import getpass
import os
import pathlib
import re
import site
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
LIB = _SCRIPTS / "lib" / "strih-program-audio.sh"
MODE_LIB = _SCRIPTS / "lib" / "program-audio-mode.sh"
TEMPLATE = _ROOT / "systemd" / "program-audio-sampler.strih-lx.service"
UNIT = "program-audio-sampler.service"
USER = getpass.getuser()

if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

_FAKE_DPKG = """#!/bin/bash
# fake dpkg-query -W -f='${Status}' PKG: installed iff PKG is listed in $FAKE_STATE/installed
pkg="${@: -1}"
grep -qxF "$pkg" "$FAKE_STATE/installed" 2>/dev/null && printf 'install ok installed' || true
"""
_FAKE_APT = """#!/bin/bash
echo "apt-get $*" >> "$FAKE_STATE/calls"
[ -e "$FAKE_STATE/apt-fails" ] && exit 100
for a in "$@"; do case "$a" in -*|install) ;; *) echo "$a" >> "$FAKE_STATE/installed" ;; esac; done
"""
_FAKE_SYSTEMCTL = """#!/bin/bash
echo "systemctl $*" >> "$FAKE_STATE/calls"
args=("$@")
[ "${args[0]}" = --user ] && args=("${args[@]:1}")
case "${args[0]}" in
  enable)
    mkdir -p "$HOME/.config/systemd/user/default.target.wants"
    for u in "${args[@]:1}"; do ln -sf "$HOME/.config/systemd/user/$u" "$HOME/.config/systemd/user/default.target.wants/$u"; done ;;
  start)
    date +%s.%N > "$FAKE_STATE/started" ;;
  is-active)
    # the operator's user manager unreachable: nothing on stdout, the error on stderr, rc 1
    [ -e "$FAKE_STATE/bus-down" ] && { echo "Failed to connect to bus: No medium found" >&2; exit 1; }
    if [ -e "$FAKE_STATE/die-after-1s" ] && [ -e "$FAKE_STATE/started" ] \
       && python3 -c 'import sys, time; sys.exit(0 if time.time() - float(open(sys.argv[1]).read()) > 1.0 else 1)' "$FAKE_STATE/started"; then
      s=failed
    else
      # one state per line, consumed one per call; the last line stays
      s="$(head -n 1 "$FAKE_STATE/active" 2>/dev/null || echo inactive)"
      [ "$(sed -n '$=' "$FAKE_STATE/active" 2>/dev/null || echo 0)" -gt 1 ] && sed -i 1d "$FAKE_STATE/active"
      : "${s:=inactive}"
    fi
    echo "$s"; [ "$s" = active ] ;;
  *) exit 0 ;;
esac
"""
_FAKE_CURL = """#!/bin/bash
echo "curl $*" >> "$FAKE_STATE/calls"
if [ -e "$FAKE_STATE/curl-fail-first" ]; then
  n="$(cat "$FAKE_STATE/curl-fail-first")"
  if [ "$n" -gt 0 ]; then echo $((n - 1)) > "$FAKE_STATE/curl-fail-first"; exit 7; fi
fi
[ -e "$FAKE_STATE/curl-body" ] || exit 7
cat "$FAKE_STATE/curl-body"
"""


class Box:
    """A fake strih-lx: a prefix for the installed files, the operator's home, a sysfs root, and the
    fake tools on PATH."""

    def __init__(self, tmp_path, ecores="12-15\n"):
        self.tmp = tmp_path
        self.home = tmp_path / "home"
        self.home.mkdir()
        self.prefix = tmp_path / "prefix"
        self.sys = tmp_path / "sys"
        if ecores is not None:
            (self.sys / "devices" / "cpu_atom").mkdir(parents=True)
            (self.sys / "devices" / "cpu_atom" / "cpus").write_text(ecores)
        self.state = tmp_path / "state"
        self.state.mkdir()
        (self.state / "installed").write_text("")
        self.bin = tmp_path / "bin"
        self.bin.mkdir()
        for name, body in (("dpkg-query", _FAKE_DPKG), ("apt-get", _FAKE_APT), ("systemctl", _FAKE_SYSTEMCTL),
                           ("curl", _FAKE_CURL)):
            (self.bin / name).write_text(body)
            (self.bin / name).chmod(0o755)

    def env(self, **extra):
        e = {
            "PATH": f"{self.bin}:/usr/local/bin:/usr/bin:/bin",
            "HOME": str(self.home),
            "FAKE_STATE": str(self.state),
            "STRIH_PROGRAM_AUDIO_PREFIX": str(self.prefix),
            "STRIH_PROGRAM_AUDIO_SYSFS": str(self.sys),
            "STRIH_PROGRAM_AUDIO_PYTHON": sys.executable,
            "STRIH_PROGRAM_AUDIO_CURL": str(self.bin / "curl"),
            "STRIH_PROGRAM_AUDIO_RETRY_SLEEP": "0",
            "LANG": "C.UTF-8",
            # HOME is the fake operator's: keep the real user site-packages (numpy on dev1 comes from
            # pip --user; strih-lx gets it from apt)
            "PYTHONUSERBASE": site.getuserbase(),
        }
        e.update(extra)
        return e

    def calls(self):
        p = self.state / "calls"
        return p.read_text().splitlines() if p.exists() else []

    def run(self, func, *args, extra_env=None):
        """`. lib; func args...` under set -euo pipefail, with verify-strih's ok/note/bad."""
        script = ('set -euo pipefail\n'
                  'ok() { printf "PASS %s\\n" "$1"; }\n'
                  'note() { printf "NOTE %s\\n" "$1"; }\n'
                  'bad() { printf "FAIL %s\\n" "$1"; }\n'
                  'lib="$1"; shift\n'
                  '. "$lib"\n'
                  '"$@"\n')
        return subprocess.run(["bash", "-c", script, "harness", str(LIB), func, *args],
                              env=self.env(**(extra_env or {})), capture_output=True, text=True, timeout=180)

    def install(self):
        return self.run("strih_program_audio_install", str(_ROOT), str(self.home), USER)

    def rows(self):
        r = self.run("strih_program_audio_grade_rows", str(_ROOT), str(self.home), USER)
        assert r.returncode == 0, r.stderr
        return [ln.split("|", 1) for ln in r.stdout.splitlines() if ln]


def _lib_value(name):
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; v="$2[*]"; printf "%s" "${!v}"', "h", str(LIB), name],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout


# ---------------------------------------------------------------------------------------------
# the unit template + its render
# ---------------------------------------------------------------------------------------------


def test_the_template_pins_the_e_cores_normal_priority_and_the_event_marker():
    t = TEMPLATE.read_text(encoding="utf-8")
    assert t.count("\nCPUAffinity=@E_CORES@\n") == 1
    assert not re.search(r"^(Nice|CPUWeight)=", t, re.M)
    marker = _lib_value("STRIH_PROGRAM_AUDIO_TEST_MARKER")
    assert re.search(rf"^ExecCondition=/usr/bin/test -e %h/{re.escape(marker)}$", t, re.M)
    assert t.count("ExecCondition=") == 1
    assert re.search(r"^ExecStart=/usr/bin/python3 /usr/local/lib/camera-box/scripts/program_audio_sampler.py$", t, re.M)
    assert re.search(r"^EnvironmentFile=-%h/\.config/camera-box/program-audio-sampler\.env$", t, re.M)
    assert re.search(r"^WantedBy=default\.target$", t, re.M)
    assert re.search(r"^Restart=on-failure$", t, re.M)
    # a user manager cannot see system targets: an After=/Wants= on one would only look like ordering
    assert not re.search(r"^(After|Wants|Requires)=.*network-online\.target", t, re.M)


@pytest.mark.parametrize("content, want", [
    ("12-15\n", "12-15"), (" 12-13,15 \n", "12-13,15"), ("", ""), ("   \n", ""), ("abc\n", ""),
    ("12-\n", ""), (",4\n", ""), (None, ""),
])
def test_the_e_cores_follow_the_lowprio_contract(tmp_path, content, want):
    """Read the way strih_lx_lowprio_prefix reads them; a value that is not a cpu list is no pin."""
    box = Box(tmp_path, ecores=content)
    r = box.run("strih_program_audio_ecores", str(box.sys))
    assert r.returncode == 0 and r.stdout == want
    if want:
        lp = subprocess.run(["bash", "-c", '. "$1"; strih_lx_lowprio_prefix "$2"', "h",
                             str(_SCRIPTS / "recording-verdict-on-strih-lx.sh"), str(box.sys)],
                            capture_output=True, text=True, timeout=30)
        assert lp.stdout == f"nice -n 19 taskset -c {want}"


def test_the_render_fills_or_drops_the_affinity_line(tmp_path):
    box = Box(tmp_path)
    t = TEMPLATE.read_text(encoding="utf-8")
    r = box.run("strih_program_audio_unit_text", str(_ROOT), "12-15")
    assert r.returncode == 0 and r.stdout == t.replace("CPUAffinity=@E_CORES@", "CPUAffinity=12-15")
    r = box.run("strih_program_audio_unit_text", str(_ROOT), "")
    assert r.returncode == 0 and r.stdout == t.replace("CPUAffinity=@E_CORES@\n", "")


def test_a_broken_template_refuses(tmp_path):
    repo = tmp_path / "repo"
    (repo / "systemd").mkdir(parents=True)
    (repo / "systemd" / TEMPLATE.name).write_text("[Service]\nCPUAffinity=0-3\n")
    box = Box(tmp_path)
    r = box.run("strih_program_audio_unit_text", str(repo), "12-15")
    assert r.returncode == 1 and "exactly one" in r.stderr


# ---------------------------------------------------------------------------------------------
# setup-strih step 16e
# ---------------------------------------------------------------------------------------------


def test_the_install_puts_everything_in_place_and_never_starts(tmp_path):
    box = Box(tmp_path)
    r = box.install()
    assert r.returncode == 0, r.stdout + r.stderr
    files = _lib_value("STRIH_PROGRAM_AUDIO_FILES").split()
    for rel in files:
        dst = box.prefix / rel
        assert dst.read_bytes() == (_ROOT / rel).read_bytes(), rel
        want = 0o755 if rel in ("scripts/program_audio_sampler.py", "scripts/build-qpsk-guard-shim.sh") else 0o644
        assert dst.stat().st_mode & 0o777 == want, rel
    unit = (box.home / ".config/systemd/user" / UNIT).read_text()
    assert re.findall(r"^CPUAffinity=.*$", unit, re.M) == ["CPUAffinity=12-15"]
    shim = box.home / _lib_value("STRIH_PROGRAM_AUDIO_SHIM")
    assert shim.is_file()
    calls = box.calls()
    assert "apt-get install -y python3-numpy" in calls
    assert f"systemctl --user enable {UNIT}" in calls
    assert "systemctl --user daemon-reload" in calls
    assert f"systemctl --user try-restart {UNIT}" in calls
    assert not any(" start " in f" {c} " and "try-restart" not in c for c in calls), calls
    assert not any(c.startswith("systemctl --user restart") for c in calls), calls


def test_a_second_install_changes_nothing(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    shim = box.home / _lib_value("STRIH_PROGRAM_AUDIO_SHIM")
    unit = box.home / ".config/systemd/user" / UNIT
    mtimes = (shim.stat().st_mtime_ns, unit.stat().st_mtime_ns)
    (box.state / "calls").write_text("")
    r = box.install()
    assert r.returncode == 0, r.stderr
    assert (shim.stat().st_mtime_ns, unit.stat().st_mtime_ns) == mtimes
    calls = box.calls()
    assert not any(c.startswith("apt-get") for c in calls)         # numpy already installed
    assert "systemctl --user daemon-reload" not in calls
    assert not any("try-restart" in c for c in calls), calls
    assert "decoder shim current" in r.stdout and "unchanged" in r.stdout


def _repo_copy(tmp_path, extra_header_line):
    """A checkout copy with every installed file + the unit template; the decoder header gains a
    line (a pull that touched the decoder)."""
    repo = tmp_path / "repo"
    for rel in _lib_value("STRIH_PROGRAM_AUDIO_FILES").split() + [f"systemd/{TEMPLATE.name}"]:
        dst = repo / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_bytes((_ROOT / rel).read_bytes())
        dst.chmod((_ROOT / rel).stat().st_mode & 0o777)
    hdr = repo / "vendor/av-sync-dock/src/camera-box-audio.hpp"
    hdr.write_bytes(hdr.read_bytes() + extra_header_line)
    return repo


def test_changed_sources_rebuild_the_shim_and_restart_a_running_sampler(tmp_path):
    """A pull that touched the decoder: the new header is installed, the shim (built from the OLD
    sources) reads stale and is rebuilt from the installed ones, and a running sampler is
    try-restarted onto both."""
    box = Box(tmp_path)
    assert box.install().returncode == 0
    repo = _repo_copy(tmp_path, b"\n// a newer revision\n")
    (box.state / "calls").write_text("")
    r = box.run("strih_program_audio_install", str(repo), str(box.home), USER)
    assert r.returncode == 0, r.stderr
    assert "decoder shim stale: building it" in r.stdout
    assert box.run("strih_program_audio_shim_state", USER, str(box.home)).stdout == "current"
    assert f"systemctl --user try-restart {UNIT}" in box.calls()
    # graded against that checkout, the box is consistent again
    rows = box.run("strih_program_audio_grade_rows", str(repo), str(box.home), USER).stdout.splitlines()
    assert [ln.split("|", 1)[0] for ln in rows] == ["OK", "OK", "NOTE"], rows


def test_no_cpu_atom_installs_without_an_affinity_line(tmp_path):
    box = Box(tmp_path, ecores=None)
    assert box.install().returncode == 0
    unit = (box.home / ".config/systemd/user" / UNIT).read_text()
    assert not re.search(r"^CPUAffinity=", unit, re.M)


def test_a_failing_apt_or_a_missing_source_fails_the_step(tmp_path):
    box = Box(tmp_path)
    (box.state / "apt-fails").write_text("")
    r = box.install()
    assert r.returncode == 1 and "apt-get install python3-numpy failed" in r.stderr
    repo = tmp_path / "repo"
    (repo / "systemd").mkdir(parents=True)
    r = box.run("strih_program_audio_install", str(repo), str(box.home), USER)
    assert r.returncode == 1 and "not found under" in r.stderr


# ---------------------------------------------------------------------------------------------
# verify-strih item 41
# ---------------------------------------------------------------------------------------------


def _marker(box):
    m = box.home / _lib_value("STRIH_PROGRAM_AUDIO_TEST_MARKER")
    m.parent.mkdir(parents=True, exist_ok=True)
    m.write_text("")
    return m


def test_a_fresh_install_grades_ok_ok_and_a_down_note(tmp_path):
    """setup-strih's own step 17 runs verify-strih right after an enable-only install: no TEST marker,
    the sampler down -- a NOTE, never a FAIL."""
    box = Box(tmp_path)
    assert box.install().returncode == 0
    rows = box.rows()
    assert [s for s, _ in rows] == ["OK", "OK", "NOTE"], rows
    assert "not in TEST mode" in rows[2][1] and "rig-mode.sh test" in rows[2][1]


def test_test_mode_but_down_fails(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    _marker(box)
    rows = box.rows()
    assert rows[2][0] == "FAIL" and "TEST mode" in rows[2][1] and "not running" in rows[2][1]


def test_a_running_sampler_must_answer_a_fresh_verdict(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    _marker(box)
    (box.state / "active").write_text("active")
    rows = box.rows()
    assert rows[2][0] == "FAIL" and "does not answer" in rows[2][1]
    assert sum("program-audio.json" in c for c in box.calls() if c.startswith("curl")) == 3   # read 3 x
    (box.state / "curl-body").write_text('{"verdict": "MEASUREMENT", "source": "S", "age_s": 0.4}')
    rows = box.rows()
    assert rows[2] == ["OK", "(program-audio-endpoint) running; http://127.0.0.1:8891/program-audio.json "
                             "answers verdict=MEASUREMENT age_s=0.4"]
    (box.state / "curl-body").write_text('{"verdict": "MEASUREMENT", "source": "S", "age_s": 30.0}')
    rows = box.rows()
    assert rows[2][0] == "FAIL" and "stale" in rows[2][1]
    (box.state / "curl-body").write_text('{"verdict": "MEASUREMENT", "source": "S", "age_s": null}')
    assert box.rows()[2][0] == "FAIL"
    (box.state / "curl-body").write_text('{"verdict": "MAYBE"}')
    assert box.rows()[2][0] == "FAIL"


@pytest.mark.parametrize("age, want", [
    (-0.5, "OK"),     # strih-lx is the dantesync date master: its nightly step can put the clock behind
    (-1.0, "OK"),
    (10.0, "OK"),
    (-1.5, "FAIL"),
    (10.5, "FAIL"),
])
def test_freshness_is_the_guards_own_window(tmp_path, age, want):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    _marker(box)
    (box.state / "active").write_text("active")
    (box.state / "curl-body").write_text('{"verdict": "MEASUREMENT", "source": "S", "age_s": %s}' % age)
    row = box.rows()[2]
    assert row[0] == want, row
    if want == "FAIL":
        assert "stale" in row[1] and "-1..10 s" in row[1]


def test_the_freshness_window_is_pinned_to_the_guard():
    import program_audio_guard as pag
    assert float(_lib_value("STRIH_PROGRAM_AUDIO_MAX_AGE_S")) == pag.DEFAULT_MAX_AGE_S
    assert float(_lib_value("STRIH_PROGRAM_AUDIO_FUTURE_TOLERANCE_S")) == pag.NEGATIVE_AGE_TOLERANCE_S


def test_running_without_the_test_marker_fails(tmp_path):
    """Running in EVENT mode: rig-mode.sh event removed the marker but the stop failed or timed out, or
    the marker was removed by hand -- the state the opt-in marker exists to prevent."""
    box = Box(tmp_path)
    assert box.install().returncode == 0
    (box.state / "active").write_text("active")
    (box.state / "curl-body").write_text('{"verdict": "MEASUREMENT", "source": "S", "age_s": 0.2}')
    row = box.rows()[2]
    assert row[0] == "FAIL" and "not in TEST mode" in row[1] and "rig-mode.sh event" in row[1], row


def test_an_unreadable_state_names_the_user_manager(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    _marker(box)
    (box.state / "bus-down").write_text("")
    row = box.rows()[2]
    assert row[0] == "FAIL" and "unreadable" in row[1] and "linger" in row[1], row
    assert "crash loop" not in row[1]


def test_the_endpoint_read_retries_a_sampler_that_is_still_binding(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    _marker(box)
    (box.state / "active").write_text("active")
    (box.state / "curl-body").write_text('{"verdict": "UNKNOWN", "source": "S", "age_s": 0.1}')
    (box.state / "curl-fail-first").write_text("2")
    assert box.rows()[2][0] == "OK"


@pytest.mark.parametrize("env, url", [
    ("PROGRAM_AUDIO_HTTP_PORT=18891\n", "http://127.0.0.1:18891/program-audio.json"),
    ("PROGRAM_AUDIO_HTTP_PORT='18892'\nPROGRAM_AUDIO_HTTP_BIND=10.77.9.202\n", "http://10.77.9.202:18892/program-audio.json"),
    ("PROGRAM_AUDIO_HTTP_BIND=0.0.0.0\n", "http://127.0.0.1:8891/program-audio.json"),
    # systemd's EnvironmentFile takes whitespace around `=` and after the value
    ("PROGRAM_AUDIO_HTTP_PORT = 18893  \n PROGRAM_AUDIO_HTTP_BIND =10.77.9.202\t\n",
     "http://10.77.9.202:18893/program-audio.json"),
])
def test_the_endpoint_follows_the_env_file(tmp_path, env, url):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    f = box.home / _lib_value("STRIH_PROGRAM_AUDIO_ENV_FILE")
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_text(env)
    _marker(box)
    (box.state / "active").write_text("active")
    (box.state / "curl-body").write_text('{"verdict": "SILENT", "source": "S", "age_s": 1.0}')
    rows = box.rows()
    assert rows[2][0] == "OK" and url in rows[2][1]
    assert any(url in c for c in box.calls())


def test_a_running_sampler_with_no_endpoint_fails(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    f = box.home / _lib_value("STRIH_PROGRAM_AUDIO_ENV_FILE")
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_text("PROGRAM_AUDIO_HTTP_PORT=0\n")
    _marker(box)
    (box.state / "active").write_text("active")
    rows = box.rows()
    assert rows[2][0] == "FAIL" and "no endpoint" in rows[2][1]


@pytest.mark.parametrize("states, want", [
    ("failed", "FAIL"),
    ("activating", "FAIL"),                      # a crash loop (auto-restart) stays activating
    ("activating\nactivating\nactive", "OK"),    # a sampler that is just starting settles
])
def test_failed_and_crash_looping_units_fail(tmp_path, states, want):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    _marker(box)
    (box.state / "active").write_text(states)
    (box.state / "curl-body").write_text('{"verdict": "MEASUREMENT", "source": "S", "age_s": 0.2}')
    row = box.rows()[2]
    assert row[0] == want, row
    if states == "activating":
        assert "crash loop" in row[1]


def test_drifted_files_or_a_stale_shim_fail(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    (box.prefix / "scripts/program_audio.py").write_text("# drifted\n")
    rows = box.rows()
    assert rows[0][0] == "FAIL" and "scripts/program_audio.py=differs" in rows[0][1]
    assert box.install().returncode == 0
    (box.home / ".config/systemd/user/default.target.wants" / UNIT).unlink()
    assert box.rows()[0][0] == "FAIL"
    assert box.install().returncode == 0
    hdr = box.prefix / "scripts/qpsk_guard_shim.cpp"
    hdr.write_bytes(hdr.read_bytes() + b"\n")
    rows = box.rows()
    assert rows[1][0] == "FAIL" and "stale" in rows[1][1]


def test_the_report_prints_every_row_through_ok_note_bad(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    r = box.run("strih_program_audio_grade_report", str(_ROOT), str(box.home), USER)
    assert r.returncode == 0, r.stderr
    assert [ln.split()[0] for ln in r.stdout.splitlines()] == ["PASS", "PASS", "NOTE"]


# ---------------------------------------------------------------------------------------------
# the file list covers what the sampler needs
# ---------------------------------------------------------------------------------------------


def _local_imports(path, seen):
    if path.name in seen:
        return
    seen.add(path.name)
    for node in ast.walk(ast.parse(path.read_text(encoding="utf-8"))):
        names = []
        if isinstance(node, ast.Import):
            names = [a.name for a in node.names]
        elif isinstance(node, ast.ImportFrom) and node.module:
            names = [node.module]
        for n in names:
            p = _SCRIPTS / f"{n.split('.')[0]}.py"
            if p.is_file():
                _local_imports(p, seen)


def test_the_installed_files_are_the_import_closure_plus_the_shim_sources():
    import program_audio_marker as pam

    seen = set()
    _local_imports(_SCRIPTS / "program_audio_sampler.py", seen)
    want = {f"scripts/{n}" for n in seen}
    want |= {str(pathlib.Path(p).resolve().relative_to(_ROOT)) for p in (pam._ROOT / s for s in pam.SOURCES)}
    want.add("scripts/build-qpsk-guard-shim.sh")
    assert set(_lib_value("STRIH_PROGRAM_AUDIO_FILES").split()) == want


def test_the_shim_path_is_the_marker_modules_default(monkeypatch):
    import program_audio_marker as pam

    assert pam.DEFAULT_SHIM_PATH == os.path.join(os.path.expanduser("~"), _lib_value("STRIH_PROGRAM_AUDIO_SHIM"))


def test_libndi_is_also_looked_up_where_strih_lx_has_it():
    import program_audio_ndi as pan

    assert "/usr/local/lib/libndi.so.6" in pan.DEFAULT_LIB_CANDIDATES


# ---------------------------------------------------------------------------------------------
# rig-mode.sh: TEST starts it, EVENT stops it (scripts/lib/program-audio-mode.sh)
# ---------------------------------------------------------------------------------------------


def _mode(func, *args, env=None):
    script = 'set -euo pipefail\nlib="$1"; shift\n. "$lib"\n"$@"\n'
    return subprocess.run(["bash", "-c", script, "harness", str(MODE_LIB), func, *args],
                          env=env, capture_output=True, text=True, timeout=60)


def _remote(tmp_path, mode, active):
    """Run the remote text the way the operator's shell on strih-lx would, with a fake systemctl."""
    box = Box(tmp_path)
    (box.state / "active").write_text(active)
    cmd = _mode("program_audio_mode_remote_cmd", mode, env=box.env()).stdout
    r = subprocess.run(["bash", "-c", cmd], env=box.env(), capture_output=True, text=True, timeout=30)
    return box, r


@pytest.mark.parametrize("active, rc0", [("active", True), ("inactive", False)])
def test_test_mode_leaves_the_marker_and_starts_the_unit(tmp_path, active, rc0):
    box, r = _remote(tmp_path, "test", active)
    assert (r.returncode == 0) is rc0 and f"program-audio-sampler: {active}" in r.stdout
    calls = box.calls()
    assert f"systemctl --user start {UNIT}" in calls
    # a start-limit hit (StartLimitBurst) is cleared first, or the start is refused for up to 300 s
    assert calls.index(f"systemctl --user reset-failed {UNIT}") < calls.index(f"systemctl --user start {UNIT}")
    assert (box.home / _lib_value("STRIH_PROGRAM_AUDIO_TEST_MARKER")).exists()


def test_test_mode_reads_the_state_after_a_settle(tmp_path):
    """A Type=simple unit reads active the moment it is forked: a sampler that dies on import a
    second later must not pass. The remote text reads is-active 2 s after the start."""
    box = Box(tmp_path)
    (box.state / "die-after-1s").write_text("")
    (box.state / "active").write_text("active")
    cmd = _mode("program_audio_mode_remote_cmd", "test", env=box.env()).stdout
    r = subprocess.run(["bash", "-c", cmd], env=box.env(), capture_output=True, text=True, timeout=30)
    assert r.returncode != 0 and "program-audio-sampler: failed" in r.stdout


@pytest.mark.parametrize("active, rc0", [("inactive", True), ("active", False)])
def test_event_mode_removes_the_marker_and_stops_the_unit(tmp_path, active, rc0):
    box = Box(tmp_path)
    m = _marker(box)
    (box.state / "active").write_text(active)
    cmd = _mode("program_audio_mode_remote_cmd", "event", env=box.env()).stdout
    r = subprocess.run(["bash", "-c", cmd], env=box.env(), capture_output=True, text=True, timeout=30)
    assert (r.returncode == 0) is rc0 and f"program-audio-sampler: {active}" in r.stdout
    calls = box.calls()
    assert f"systemctl --user stop {UNIT}" in calls
    # `stop` leaves a failed unit failed: item 41 would FAIL in EVENT mode until a reboot
    assert calls.index(f"systemctl --user stop {UNIT}") < calls.index(f"systemctl --user reset-failed {UNIT}")
    assert not m.exists()


def test_an_unknown_mode_is_refused():
    assert _mode("program_audio_mode_remote_cmd", "prod").returncode == 1


_FAKE_SSHPASS = """#!/bin/bash
# fake sshpass -p PW timeout T ssh OPTS... USER@HOST CMD: record, then run CMD here
echo "sshpass $*" >> "$FAKE_STATE/ssh"
[ -e "$FAKE_STATE/ssh-fails" ] && { echo "ssh: connect to host refused" >&2; exit 255; }
exec bash -c "${@: -1}"
"""


def _apply(tmp_path, mode, platform="linux", active="active", ssh_fails=False):
    box = Box(tmp_path)
    (box.bin / "sshpass").write_text(_FAKE_SSHPASS)
    (box.bin / "sshpass").chmod(0o755)
    (box.state / "active").write_text(active)
    if ssh_fails:
        (box.state / "ssh-fails").write_text("")
    r = _mode("program_audio_mode_apply", mode, "10.77.9.202", env=box.env(STRIH_PLATFORM=platform))
    ssh = (box.state / "ssh").read_text() if (box.state / "ssh").exists() else ""
    return box, r, ssh


def test_apply_runs_the_remote_text_as_the_operator_over_ssh(tmp_path):
    box, r, ssh = _apply(tmp_path, "test")
    assert r.returncode == 0 and "[program-audio 10.77.9.202] test: program-audio-sampler: active" in r.stdout
    assert ssh.startswith("sshpass -p newlevel timeout 30 ssh ") and "newlevel@10.77.9.202" in ssh
    assert "UserKnownHostsFile=/dev/null" in ssh


def test_apply_on_a_windows_strih_is_one_skip_line_and_no_ssh(tmp_path):
    _box, r, ssh = _apply(tmp_path, "event", platform="windows")
    assert r.returncode == 0 and "SKIP" in r.stdout and ssh == ""


@pytest.mark.parametrize("mode, active, says", [
    ("test", "inactive", "did not start"),
    ("event", "active", "did not stop"),
])
def test_apply_failures_are_warnings_never_the_exit_status(tmp_path, mode, active, says):
    _box, r, _ssh = _apply(tmp_path, mode, active=active)
    assert r.returncode == 0 and says in r.stderr and "WARNING" in r.stderr


def test_an_unreachable_strih_is_a_warning(tmp_path):
    _box, r, _ssh = _apply(tmp_path, "test", ssh_fails=True)
    assert r.returncode == 0 and "did not start" in r.stderr and "refused" in r.stderr


# ---------------------------------------------------------------------------------------------
# the wiring
# ---------------------------------------------------------------------------------------------


def test_setup_strih_runs_step_16e_and_verify_strih_grades_it():
    setup = (_SCRIPTS / "setup-strih.sh").read_text(encoding="utf-8")
    assert setup.count('strih_program_audio_install "${HERE}/.." "$USER_HOME" "$DESKTOP_USER"') == 1
    assert setup.index('step "16e"') < setup.index('strih_program_audio_install "${HERE}/..') < setup.index("step 17 ")
    verify = (_SCRIPTS / "verify-strih.sh").read_text(encoding="utf-8")
    assert verify.count('strih_program_audio_grade_report "${HERE}/.." "$USER_HOME"') == 1
    prov = (_SCRIPTS / "lib" / "strih-provision.sh").read_text(encoding="utf-8")
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-program-audio.sh"' in prov


def test_rig_mode_starts_it_in_test_and_stops_it_in_event():
    s = (_SCRIPTS / "rig-mode.sh").read_text(encoding="utf-8")
    assert s.count('. "$RIG_MODE_DIR/lib/program-audio-mode.sh"') == 1
    test_body = s[s.index("\ndo_test() {"):s.index("\ndo_event() {")]
    event_body = s[s.index("\ndo_event() {"):]
    event_body = event_body[:event_body.index("\n}\n")]
    assert test_body.count('program_audio_mode_apply test "$STRIH_IP"') == 1
    assert event_body.count('program_audio_mode_apply event "$STRIH_IP"') == 1
    assert 'program_audio_mode_apply event' not in test_body and 'program_audio_mode_apply test' not in event_body
