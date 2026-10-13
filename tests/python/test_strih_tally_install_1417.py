"""issue 1417: setup-strih step 16f installs strih-tally, verify-strih item 42 grades it.

Pinned here:
  * the two system units (strih-tally.service from its @USER@ template, strih-tally-mdns.service
    announcing atemrpi.local through avahi-publish from /etc/default/strih-tally);
  * `strih_tally_install` (scripts/lib/strih-tally.sh): packages only when missing, the two python
    files + both units + the default file (STRIH_TALLY_IP = the box's STRIH_IP) written only when they
    differ, daemon-reload only after a write, try-restart of a running unit whose files changed, then
    enable --now (or start, once enabled) -- tally is production function, not enable-only;
  * `strih_tally_grade_rows` (4 rows): units + files, :9240 /state with obs_connected, atemrpi.local
    resolving to exactly this box, the service's program scene equal to the strih OBS's own;
  * the role gate (16f = the strih role only) and the setup / verify wiring within the line budget.

Every bash run sources the libs under the caller's `set -euo pipefail`; values reach bash as
ARGUMENTS, never inside the script text. Fake dpkg-query / apt-get / systemctl / resolver / OBS
reader; a real local HTTP server for /state. No root, no rig.
"""
import http.server
import json
import os
import socket
import subprocess
import threading
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-tally.sh"
SESSION = SCRIPTS / "lib" / "strih-session-apps.sh"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
ROLE = SCRIPTS / "lib" / "obs-box-role.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
UNIT_TPL = REPO / "systemd" / "strih-tally.service.in"
MDNS_UNIT = REPO / "systemd" / "strih-tally-mdns.service"
USER = "operator1"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_BOXES_DIR", "OBS_FLEET")

_FAKE_DPKG = """#!/bin/bash
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
case "$1" in
  enable)
    shift; [ "$1" = --now ] && { now=1; shift; }
    for u in "$@"; do touch "$FAKE_STATE/enabled-$u"; [ -n "${now:-}" ] && touch "$FAKE_STATE/active-$u"; done ;;
  start)
    shift; for u in "$@"; do touch "$FAKE_STATE/active-$u"; done ;;
  is-enabled)
    if [ -e "$FAKE_STATE/enabled-$2" ]; then echo enabled; else echo disabled; exit 1; fi ;;
  is-active)
    if [ -e "$FAKE_STATE/active-$2" ]; then echo active; else echo inactive; exit 3; fi ;;
esac
exit 0
"""
_FAKE_PYTHON = """#!/bin/bash
echo "python $*" >> "$FAKE_STATE/calls"
[ -e "$FAKE_STATE/python-fails" ] && exit 1
exit 0
"""
_FAKE_RESOLVE = """#!/bin/bash
cat "$FAKE_STATE/resolve" 2>/dev/null || true
"""
_FAKE_OBS_PROGRAM = """#!/bin/bash
cat "$FAKE_STATE/obs-program" 2>/dev/null || true
"""


def _write_exec(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


class _StateHandler(http.server.BaseHTTPRequestHandler):
    body = b"{}"

    def do_GET(self):  # noqa: N802 -- http.server API
        if self.path != "/state":
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(self.server.body)

    def log_message(self, *args):  # quiet
        return


class Box:
    def __init__(self, tmp_path):
        self.tmp = tmp_path
        self.state = tmp_path / "state"
        self.state.mkdir()
        (self.state / "installed").write_text("python3-websockets\npython3-websocket\navahi-utils\n")
        self.bin = tmp_path / "bin"
        self.bin.mkdir()
        _write_exec(self.bin / "dpkg-query", _FAKE_DPKG)
        _write_exec(self.bin / "apt-get", _FAKE_APT)
        self.systemctl = _write_exec(tmp_path / "fake-systemctl", _FAKE_SYSTEMCTL)
        self.python = _write_exec(tmp_path / "fake-python", _FAKE_PYTHON)
        self.resolver = _write_exec(tmp_path / "fake-resolve", _FAKE_RESOLVE)
        self.obs_reader = _write_exec(tmp_path / "fake-obs-program", _FAKE_OBS_PROGRAM)
        self.prefix = tmp_path / "prefix"
        self.units = tmp_path / "units"
        self.units.mkdir()
        self.default = tmp_path / "default" / "strih-tally"
        self.server = None
        self.state_url = "http://127.0.0.1:%d/state" % _closed_port()

    def serve_state(self, payload):
        srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _StateHandler)
        srv.body = json.dumps(payload).encode()
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.server = srv
        self.state_url = "http://127.0.0.1:%d/state" % srv.server_address[1]

    def close(self):
        if self.server is not None:
            self.server.shutdown()
            self.server.server_close()

    def env(self):
        e = {k: v for k, v in os.environ.items() if k not in LEAKY}
        e.update({
            "PATH": "%s:%s" % (self.bin, e["PATH"]),
            "FAKE_STATE": str(self.state),
            "STRIH_TALLY_PREFIX": str(self.prefix),
            "STRIH_TALLY_UNIT_DIR": str(self.units),
            "STRIH_TALLY_DEFAULT_FILE": str(self.default),
            "STRIH_TALLY_SYSTEMCTL": str(self.systemctl),
            "STRIH_TALLY_PYTHON": str(self.python),
            "STRIH_TALLY_STATE_URL": self.state_url,
            "STRIH_TALLY_RESOLVE_CMD": "%s -4 -n atemrpi.local" % self.resolver,
            "STRIH_TALLY_OBS_PROGRAM_CMD": str(self.obs_reader),
            "STRIH_TALLY_RETRY_SLEEP": "0",
        })
        return e

    def run(self, func, *args, box="strih-snv", repo=REPO):
        script = ('set -euo pipefail\n. "$1"\n. "$2"\nstrih_box_load "$3"\n. "$4"\n'
                  'func="$5"; shift 5\n"$func" "$@"\n')
        return subprocess.run(["bash", "-c", script, "harness", str(SESSION), str(FACTS), box, str(LIB),
                               func, *args], capture_output=True, text=True, env=self.env(), timeout=60)

    def install(self, repo=REPO, box="strih-snv"):
        return self.run("strih_tally_install", str(repo), USER, box=box)

    def rows(self, box="strih-snv"):
        r = self.run("strih_tally_grade_rows", str(REPO), USER, box=box)
        assert r.returncode == 0, r.stderr
        return [ln.split("|", 1) for ln in r.stdout.splitlines() if ln]

    def calls(self):
        p = self.state / "calls"
        return p.read_text().splitlines() if p.exists() else []

    def reset_calls(self):
        (self.state / "calls").write_text("")


def _closed_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


@pytest.fixture
def box(tmp_path):
    b = Box(tmp_path)
    yield b
    b.close()


def _healthy(box):
    assert box.install().returncode == 0
    box.serve_state({"pgm": 3, "pvw": 5, "program_scene": "Cam 3", "preview_scene": "Cam 5",
                     "studio_mode": True, "obs_connected": True, "camera_inputs": 7,
                     "tallies": ["10.77.9.50 ESP-3"]})
    (box.state / "resolve").write_text("atemrpi.local\t10.77.9.202\n")
    (box.state / "obs-program").write_text("Cam 3\n")


# --- the units ---------------------------------------------------------------------------------------

def test_the_tally_unit_template_runs_the_installed_service_as_the_operator():
    t = UNIT_TPL.read_text()
    assert t.count("User=@USER@") == 1
    assert "ExecStart=/usr/bin/python3 /usr/local/lib/camera-box/strih_tally.py --port 9240 --obs ws://127.0.0.1:4455" in t
    assert "Restart=always" in t and "WantedBy=multi-user.target" in t


def test_both_units_are_hardened():
    for unit in (UNIT_TPL, MDNS_UNIT):
        t = unit.read_text()
        for line in ("NoNewPrivileges=yes", "ProtectSystem=strict", "ProtectHome=yes", "PrivateTmp=yes"):
            assert line + "\n" in t, (unit.name, line)


def test_the_mdns_unit_announces_atemrpi_local_at_the_box_ip():
    t = MDNS_UNIT.read_text()
    assert "EnvironmentFile=/etc/default/strih-tally" in t
    assert "ExecStart=/usr/bin/avahi-publish -a -R atemrpi.local ${STRIH_TALLY_IP}" in t
    assert "Requires=avahi-daemon.service" in t and "Restart=always" in t


# --- setup-strih step 16f -----------------------------------------------------------------------------

def test_a_first_install_writes_everything_reloads_and_starts_both_units(box):
    r = box.install()
    assert r.returncode == 0, r.stderr
    for name in ("strih_tally.py", "strih_tally_decision.py"):
        assert (box.prefix / name).read_bytes() == (SCRIPTS / name).read_bytes()
        assert oct((box.prefix / name).stat().st_mode & 0o777) == "0o644"
    unit = (box.units / "strih-tally.service").read_text()
    assert "User=%s\n" % USER in unit and "@USER@" not in unit
    assert (box.units / "strih-tally-mdns.service").read_bytes() == MDNS_UNIT.read_bytes()
    assert box.default.read_text() == "STRIH_TALLY_IP=10.77.9.202\n"
    calls = box.calls()
    assert "systemctl daemon-reload" in calls
    assert "systemctl enable --now strih-tally.service strih-tally-mdns.service" in calls
    assert calls.index("systemctl daemon-reload") < calls.index(
        "systemctl enable --now strih-tally.service strih-tally-mdns.service")
    assert not any(c.startswith("apt-get") for c in calls)
    assert r.stdout.count("written") == 5


def test_strih_pp_gets_its_own_ip(box):
    assert box.install(box="strih-pp").returncode == 0
    assert box.default.read_text() == "STRIH_TALLY_IP=10.77.8.202\n"


def test_a_second_install_writes_nothing_and_does_not_reload(box):
    assert box.install().returncode == 0
    box.reset_calls()
    r = box.install()
    assert r.returncode == 0, r.stderr
    assert "written" not in r.stdout
    calls = box.calls()
    assert "systemctl daemon-reload" not in calls
    assert not any(c.startswith("systemctl try-restart") for c in calls)
    assert not any(c.startswith("systemctl enable") for c in calls)
    assert "systemctl start strih-tally.service strih-tally-mdns.service" in calls


def test_a_changed_service_file_restarts_only_the_tally_unit(box, tmp_path):
    assert box.install().returncode == 0
    repo = tmp_path / "repo"
    (repo / "scripts").mkdir(parents=True)
    (repo / "systemd").mkdir()
    for name in ("strih_tally.py", "strih_tally_decision.py"):
        (repo / "scripts" / name).write_bytes((SCRIPTS / name).read_bytes())
    (repo / "scripts" / "strih_tally.py").write_text((SCRIPTS / "strih_tally.py").read_text() + "\n# changed\n")
    for unit in (UNIT_TPL, MDNS_UNIT):
        (repo / "systemd" / unit.name).write_bytes(unit.read_bytes())
    box.reset_calls()
    r = box.install(repo=repo)
    assert r.returncode == 0, r.stderr
    calls = box.calls()
    assert "systemctl daemon-reload" in calls
    assert "systemctl try-restart strih-tally.service" in calls
    assert calls.index("systemctl daemon-reload") < calls.index("systemctl try-restart strih-tally.service")


def test_missing_packages_are_installed_and_an_apt_failure_fails_the_step(box):
    (box.state / "installed").write_text("")
    r = box.install()
    assert r.returncode == 0, r.stderr
    assert any(c.startswith("apt-get install -y") and "python3-websockets" in c and "avahi-utils" in c
               and "python3-websocket " in c + " " for c in box.calls())
    (box.state / "installed").write_text("")
    (box.state / "apt-fails").write_text("")
    r = box.install()
    assert r.returncode != 0
    assert "apt-get install" in r.stderr


def test_a_python_that_cannot_import_websockets_fails_before_any_write(box):
    (box.state / "python-fails").write_text("")
    r = box.install()
    assert r.returncode != 0
    assert "websockets" in r.stderr
    assert not (box.units / "strih-tally.service").exists()


# --- verify-strih item 42 -----------------------------------------------------------------------------

def test_a_healthy_box_grades_four_ok_rows(box):
    _healthy(box)
    rows = box.rows()
    assert [r[0] for r in rows] == ["OK", "OK", "OK", "OK"], rows
    text = "\n".join(r[1] for r in rows)
    for tag in ("(tally-units)", "(tally-port)", "(tally-mdns)", "(tally-obs)"):
        assert tag in text
    assert box.run("strih_tally_row_count").stdout == "4"


def test_units_not_running_or_drifted_files_fail(box):
    _healthy(box)
    (box.state / "active-strih-tally-mdns.service").unlink()
    rows = dict((r[1].split(")")[0] + ")", r[0]) for r in box.rows())
    assert rows["(tally-units)"] == "FAIL"
    (box.state / "active-strih-tally-mdns.service").write_text("")
    (box.prefix / "strih_tally.py").write_text("# drifted\n")
    rows = box.rows()
    assert rows[0][0] == "FAIL" and "strih_tally.py=differs" in rows[0][1]


@pytest.mark.parametrize("resolved, word", [
    ("atemrpi.local\t10.77.9.77\n", "10.77.9.77"),
    ("atemrpi.local\t10.77.9.202\natemrpi.local\t10.77.9.77\n", "10.77.9.77"),
    ("", "does not resolve"),
])
def test_atemrpi_local_must_resolve_to_exactly_this_box(box, resolved, word):
    _healthy(box)
    (box.state / "resolve").write_text(resolved)
    row = box.rows()[2]
    assert row[0] == "FAIL" and "(tally-mdns)" in row[1] and word in row[1], row


def test_an_unreachable_state_endpoint_fails_the_port_and_obs_rows(box):
    _healthy(box)
    box.close()
    box.server = None
    box.state_url = "http://127.0.0.1:%d/state" % _closed_port()
    rows = box.rows()
    assert rows[1][0] == "FAIL" and "(tally-port)" in rows[1][1] and "does not answer" in rows[1][1]
    assert rows[3][0] == "FAIL" and "(tally-obs)" in rows[3][1]


def test_obs_not_connected_fails_the_port_row(box):
    _healthy(box)
    box.close()
    box.serve_state({"pgm": 0, "pvw": 0, "program_scene": None, "preview_scene": None,
                     "studio_mode": False, "obs_connected": False, "tallies": []})
    row = box.rows()[1]
    assert row[0] == "FAIL" and "obs_connected=false" in row[1]


def test_a_program_scene_that_differs_from_obs_fails(box):
    _healthy(box)
    (box.state / "obs-program").write_text("Cam 4\n")
    row = box.rows()[3]
    assert row[0] == "FAIL" and "Cam 3" in row[1] and "Cam 4" in row[1]


def test_the_report_prints_every_row_through_ok_note_bad(box):
    _healthy(box)
    script = ('set -euo pipefail\n. "$1"\n. "$2"\nstrih_box_load strih-snv\n. "$3"\n'
              'ok() { echo "PASS $1"; }\nnote() { echo "NOTE $1"; }\nbad() { echo "FAIL $1"; }\n'
              'strih_tally_grade_report "$4" "$5"\n')
    r = subprocess.run(["bash", "-c", script, "h", str(SESSION), str(FACTS), str(LIB), str(REPO), USER],
                       capture_output=True, text=True, env=box.env(), timeout=60)
    assert r.returncode == 0, r.stderr
    assert [ln.split()[0] for ln in r.stdout.splitlines()] == ["PASS"] * 4


# --- the role gate + the wiring ----------------------------------------------------------------------

def _applies(box_name, step="16f"):
    script = ('set -euo pipefail\n. "$1"\n. "$2"\nstrih_box_load "$3"\n'
              'if WHY="$(obs_box_step_applies "$4")"; then echo RUN; else echo "SKIP $WHY"; fi\n')
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    r = subprocess.run(["bash", "-c", script, "h", str(FACTS), str(ROLE), box_name, step],
                       capture_output=True, text=True, env=e, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_16f_runs_on_the_strih_role_only():
    assert _applies("strih-snv") == "RUN"
    assert _applies("strih-pp") == "RUN"
    assert _applies("stream-pp") == "SKIP the stream role runs no camera tally (no camera operators on stream-pp)"


def test_setup_and_verify_call_the_lib_once_within_the_line_budget():
    s = SETUP.read_text()
    v = VERIFY.read_text()
    assert len(s.splitlines()) <= 1000 and len(v.splitlines()) <= 1000
    assert s.count('strih_tally_install "${HERE}/.." "$DESKTOP_USER"') == 1
    g = s.index('if WHY="$(obs_box_step_applies 16f)"; then')
    assert s.index('step "16e"') < s.index('step "16f"') < g < s.index('strih_tally_install "${HERE}/..') \
        < s.index("step 17 ")
    assert v.count('strih_tally_grade_report "${HERE}/.." "${STRIH_LX_USER:-newlevel}"') == 1
    i = v.index("strih_tally_grade_report")
    assert v.index("strih_program_audio_grade_report") < i
    assert v.index('if WHY="$(obs_box_step_applies 16f)"; then') < i
    # never between item 34 and 32: test_ndi_discovery_1342 runs that slice with only its own lib sourced
    assert not (v.index("# 34) NDI discovery") < i < v.index("# 32) the shared OBS-box"))
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-tally.sh"' in PROVISION.read_text()


def test_a_service_with_no_mapped_camera_fails_the_port_row(box):
    """A strih where no NDI input carries a CAM<n> source lights nothing; the grade must say so."""
    _healthy(box)
    box.close()
    box.serve_state({"pgm": 0, "pvw": 0, "program_scene": "Cam 3", "preview_scene": "Cam 5",
                     "studio_mode": True, "obs_connected": True, "camera_inputs": 0, "tallies": []})
    row = box.rows()[1]
    assert row[0] == "FAIL" and "(tally-port)" in row[1] and "no camera" in row[1], row
