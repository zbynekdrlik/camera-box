"""issue 1474 -- the Linux stream box keeps the `dev1` rig-lease host line in /etc/hosts itself.

restreamer's rig-lease client on stream-snv dials http://dev1:8890/rig-lease.json; the box has no
tailscale, so `dev1` resolved nowhere and the client failed OPEN. dev1 is reachable as dev1.local over
mDNS, its rig address moves by DHCP. scripts/lib/obs-box-lease-host.sh renders a root refresh script +
oneshot + timer (setup-strih step 16l) that keeps EXACTLY one managed hosts line, and the verify-strih
grade. Hermetic: getent / systemctl / curl / dpkg-query are fakes on PATH, every path is a temp file."""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
BASELINE = SCRIPTS / "lib" / "obs-box-baseline.sh"

STOPGAP = "10.77.9.195\tdev1\t# camera-box issue 1474: the rig lease host, refreshed from dev1.local\n"
GETENT_LIVE = (
    "10.77.9.195     STREAM dev1.local\n10.77.9.195     DGRAM  \n10.77.9.195     RAW    \n")


def _exec(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


class Rig:
    """Fake tools on PATH: getent answers from files, systemctl/apt-get log their argv, curl prints a code."""

    def __init__(self, tmp, ahosts=GETENT_LIVE, hosts_answer="10.77.9.195     dev1\n", code="200",
                 timer="enabled/active", nss_installed=True, apt_rc=0):
        self.tmp = tmp
        self.bin = tmp / "bin"
        self.bin.mkdir()
        self.log = tmp / "calls"
        (tmp / "ahosts").write_text(ahosts)
        (tmp / "hosts-answer").write_text(hosts_answer)
        en, act = timer.split("/")
        _exec(self.bin / "getent", '#!/bin/bash\necho "getent $*" >> %s\n'
              'case "$1" in ahostsv4) [ "$2" = dev1.local ] && cat %s/ahosts;; hosts) [ "$2" = dev1 ] && cat %s/hosts-answer;; esac\n'
              'exit 0\n' % (self.log, tmp, tmp))
        _exec(self.bin / "systemctl", '#!/bin/bash\necho "systemctl $*" >> %s\n'
              'case "$1" in is-enabled) echo %s;; is-active) echo %s;; start) [ -e %s/start-fails ] && exit 1;; esac\nexit 0\n'
              % (self.log, en, act, tmp))
        _exec(self.bin / "curl", '#!/bin/bash\necho "curl $*" >> %s\nprintf %s\n' % (self.log, code))
        _exec(self.bin / "dpkg-query", '#!/bin/bash\n%s\n' % (
            'printf "install ok installed"' if nss_installed else 'echo "no packages found" >&2; exit 1'))
        _exec(self.bin / "apt-get", '#!/bin/bash\necho "apt-get $*" >> %s\nexit %d\n' % (self.log, apt_rc))
        self.sbin = tmp / "sbin"
        self.units = tmp / "units"
        self.sbin.mkdir()
        self.units.mkdir()

    def env(self):
        return {"PATH": "%s:%s" % (self.bin, os.environ["PATH"]),
                "OBS_BOX_LEASE_SBIN_DIR": str(self.sbin), "OBS_BOX_LEASE_UNIT_DIR": str(self.units),
                "OBS_BOX_LEASE_OWNER": "%s:%s" % (_id("-un"), _id("-gn"))}

    def calls(self):
        return self.log.read_text() if self.log.exists() else ""


def _id(flag):
    return subprocess.run(["id", flag], capture_output=True, text=True, check=True).stdout.strip()


def _bash(body, box="stream-snv", env=None, stdin=None):
    script = ('set -euo pipefail\nfail(){ echo "FAIL: $*" >&2; exit 1; }\nwarn(){ echo "WARN: $*"; }\n'
              'ok(){ echo "OK $*"; }\nbad(){ echo "BAD $*"; }\nnote(){ echo "NOTE $*"; }\n'
              '. "$1"\nstrih_box_load "$2"\n. "$3"\n. "$4"\n' + body)
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "h", str(FACTS), box, str(PROVISION), str(BASELINE)],
                          capture_output=True, text=True, env=e, timeout=60, input=stdin)


# --- the gate ---------------------------------------------------------------------------------------
def test_step_16l_applies_to_the_snv_stream_box_only():
    assert _bash("obs_box_step_applies 16l-lease").returncode == 0
    for box, word in (("stream-pp", "STRIH_VENUE=pp"), ("strih-snv", "strih role"), ("strih-pp", "strih role")):
        r = _bash("obs_box_step_applies 16l-lease", box=box)
        assert r.returncode == 1 and word in r.stdout, (box, r.stdout)


# --- the pure planner ---------------------------------------------------------------------------------
def _pick(answer, box="10.77.9.204", prefix="23"):
    return _bash('obs_box_lease_host_pick %s %s' % (box, prefix), stdin=answer)


def test_pick_takes_the_first_address_inside_the_rig_network():
    answer = "100.104.8.125 STREAM dev1.local\n172.17.0.1 STREAM\n10.77.8.40 STREAM\n10.77.9.195 STREAM\n"
    r = _pick(answer)
    assert r.returncode == 0 and r.stdout == "10.77.8.40\n"


def test_pick_refuses_an_answer_outside_the_rig_network_or_empty():
    for answer in ("100.104.8.125 STREAM dev1.local\n10.77.10.1 STREAM\n", "", "garbage line\n999.1.1.1 X\n"):
        r = _pick(answer)
        assert r.returncode == 1 and r.stdout == "", (answer, r.stdout)
    assert _pick("10.77.9.204 STREAM\n").returncode == 1  # never the box itself
    assert _pick("10.77.9.195 STREAM\n", prefix="x").returncode == 1


def _render(ip, text):
    r = _bash("obs_box_lease_host_render %s; printf x" % ip, stdin=text)
    assert r.returncode == 0, r.stderr
    return r.stdout[:-1]


def test_the_rendered_line_is_the_live_stopgap_byte_for_byte():
    base = "127.0.0.1\tlocalhost\n127.0.1.1\tstream-snv\n"
    assert _render("10.77.9.195", base) == base + STOPGAP
    assert _render("10.77.9.195", base + STOPGAP) == base + STOPGAP


def test_render_replaces_in_place_drops_duplicates_and_keeps_every_other_line():
    text = "127.0.0.1 localhost\n" + STOPGAP + "# a comment\n" + STOPGAP + "::1 ip6-localhost\n"
    out = _render("10.77.8.77", text)
    assert out == ("127.0.0.1 localhost\n" + STOPGAP.replace("10.77.9.195", "10.77.8.77")
                   + "# a comment\n::1 ip6-localhost\n")


def test_render_adds_a_missing_final_newline_before_appending():
    assert _render("10.77.9.195", "127.0.0.1 localhost") == "127.0.0.1 localhost\n" + STOPGAP


def test_foreign_names_only_an_unmanaged_uncommented_dev1_host():
    text = ("10.77.9.109 dev1\n#10.1.1.1 dev1\n10.2.2.2 dev10 foo # dev1\n" + STOPGAP
            + "10.3.3.3 a dev1 b\n")
    r = _bash("obs_box_lease_host_foreign", stdin=text)
    assert r.stdout == "10.77.9.109 dev1\n10.3.3.3 a dev1 b\n"


# --- the rendered refresh script ------------------------------------------------------------------------
def _script(tmp, env=None):
    r = _bash("obs_box_lease_host_script_text", env=env)
    assert r.returncode == 0, r.stderr
    return _exec(tmp / "refresh", r.stdout)


def _run(script, hosts, rig):
    return subprocess.run(["bash", str(script), str(hosts)], capture_output=True, text=True,
                          env=dict(os.environ, **rig.env()), timeout=60)


def test_script_bakes_the_box_ip_and_is_strict():
    r = _bash("obs_box_lease_host_script_text")
    lines = r.stdout.splitlines()
    assert lines[0] == "#!/usr/bin/env bash" and "set -euo pipefail" in lines
    assert lines[-1] == 'obs_box_lease_host_refresh "${1:-/etc/hosts}" 10.77.9.204 23'
    assert subprocess.run(["bash", "-n", "-c", r.stdout]).returncode == 0


def test_refresh_writes_the_line_atomically_and_keeps_the_mode(tmp_path):
    rig = Rig(tmp_path)
    hosts = tmp_path / "hosts"
    hosts.write_text("127.0.0.1 localhost\n10.77.9.1 dev1x\n")
    hosts.chmod(0o644)
    ino = hosts.stat().st_ino
    r = _run(_script(tmp_path), hosts, rig)
    assert r.returncode == 0, r.stderr
    assert hosts.read_text() == "127.0.0.1 localhost\n10.77.9.1 dev1x\n" + STOPGAP
    assert hosts.stat().st_ino != ino and oct(hosts.stat().st_mode & 0o777) == "0o644"
    assert "written" in r.stdout
    assert not [p for p in tmp_path.iterdir() if ".lease-host." in p.name]


def test_refresh_leaves_an_unchanged_file_alone(tmp_path):
    rig = Rig(tmp_path)
    hosts = tmp_path / "hosts"
    hosts.write_text("127.0.0.1 localhost\n" + STOPGAP)
    ino = hosts.stat().st_ino
    r = _run(_script(tmp_path), hosts, rig)
    assert r.returncode == 0 and "unchanged" in r.stdout
    assert hosts.stat().st_ino == ino


def test_refresh_follows_dev1_to_a_new_address(tmp_path):
    rig = Rig(tmp_path, ahosts="10.77.9.231 STREAM dev1.local\n")
    hosts = tmp_path / "hosts"
    hosts.write_text("127.0.0.1 localhost\n" + STOPGAP)
    assert _run(_script(tmp_path), hosts, rig).returncode == 0
    assert hosts.read_text() == "127.0.0.1 localhost\n" + STOPGAP.replace("10.77.9.195", "10.77.9.231")


def test_a_failed_resolution_leaves_the_file_and_fails(tmp_path):
    for n, answer in enumerate(("", "100.104.8.125 STREAM dev1.local\n")):
        case = tmp_path / str(n)
        case.mkdir()
        rig = Rig(case, ahosts=answer)
        hosts = case / "hosts"
        hosts.write_text("127.0.0.1 localhost\n" + STOPGAP)
        ino = hosts.stat().st_ino
        r = _run(_script(case), hosts, rig)
        assert r.returncode == 1 and "left as it is" in r.stderr, r.stderr
        assert hosts.read_text() == "127.0.0.1 localhost\n" + STOPGAP and hosts.stat().st_ino == ino


def test_a_foreign_dev1_line_is_kept_and_named(tmp_path):
    rig = Rig(tmp_path)
    hosts = tmp_path / "hosts"
    hosts.write_text("10.77.9.109 dev1\n")
    r = _run(_script(tmp_path), hosts, rig)
    assert r.returncode == 0
    assert hosts.read_text() == "10.77.9.109 dev1\n" + STOPGAP
    assert "unmanaged line" in r.stderr and "10.77.9.109 dev1" in r.stderr


# --- review round 1: a failed step never writes, edge inputs ---------------------------------------------
def _hosts(tmp, text="127.0.0.1 localhost\n10.77.9.1 gw\n"):
    hosts = tmp / "hosts"
    hosts.write_text(text)
    hosts.chmod(0o644)
    return hosts


def _unchanged(hosts, text, ino):
    assert hosts.read_text() == text and hosts.stat().st_ino == ino


def test_a_failing_render_never_truncates_the_hosts_file(tmp_path):
    rig = Rig(tmp_path)
    script = _script(tmp_path)
    _exec(rig.bin / "awk", '#!/bin/bash\nexit 2\n')
    hosts = _hosts(tmp_path)
    ino = hosts.stat().st_ino
    r = _run(script, hosts, rig)
    assert r.returncode == 1 and "written" not in r.stdout, (r.stdout, r.stderr)
    _unchanged(hosts, "127.0.0.1 localhost\n10.77.9.1 gw\n", ino)


def test_a_render_that_drops_lines_is_refused(tmp_path):
    rig = Rig(tmp_path)
    script = _script(tmp_path)
    _exec(rig.bin / "awk", '#!/bin/bash\ncat >/dev/null\nexit 0\n')  # prints nothing, exit 0
    hosts = _hosts(tmp_path)
    ino = hosts.stat().st_ino
    r = _run(script, hosts, rig)
    assert r.returncode == 1 and "refused" in r.stderr, r.stderr
    _unchanged(hosts, "127.0.0.1 localhost\n10.77.9.1 gw\n", ino)


def test_a_render_without_the_managed_line_is_refused(tmp_path):
    rig = Rig(tmp_path)
    script = _script(tmp_path)
    _exec(rig.bin / "awk", '#!/bin/bash\nexec cat\n')  # returns the input unchanged, exit 0
    hosts = _hosts(tmp_path)
    ino = hosts.stat().st_ino
    r = _run(script, hosts, rig)
    assert r.returncode == 1 and "refused" in r.stderr and "unchanged" not in r.stdout, (r.stdout, r.stderr)
    _unchanged(hosts, "127.0.0.1 localhost\n10.77.9.1 gw\n", ino)


def test_an_unreadable_hosts_path_fails_at_the_read(tmp_path):
    rig = Rig(tmp_path)
    script = _script(tmp_path)
    for target in (tmp_path / "a-directory", tmp_path / "missing"):
        if target.name == "a-directory":
            target.mkdir()
        r = _run(script, target, rig)
        assert r.returncode == 1 and "cannot read" in r.stderr, (target, r.stderr)
        assert not [p for p in tmp_path.iterdir() if ".lease-host." in p.name]
    assert not (tmp_path / "missing").exists()


def test_a_failed_write_leaves_no_temp_file_and_the_hosts_file_unchanged(tmp_path):
    rig = Rig(tmp_path)
    script = _script(tmp_path)
    _exec(rig.bin / "mv", '#!/bin/bash\nexit 1\n')
    hosts = _hosts(tmp_path)
    ino = hosts.stat().st_ino
    r = _run(script, hosts, rig)
    assert r.returncode == 1 and "cannot write" in r.stderr
    _unchanged(hosts, "127.0.0.1 localhost\n10.77.9.1 gw\n", ino)
    assert not [p for p in tmp_path.iterdir() if ".lease-host." in p.name]


def test_a_temp_file_a_killed_run_left_is_swept(tmp_path):
    rig = Rig(tmp_path)
    hosts = _hosts(tmp_path, "127.0.0.1 localhost\n" + STOPGAP)
    stale = tmp_path / ".hosts.lease-host.AbC123"
    stale.write_text("half")
    keep = tmp_path / ".hosts.other"
    keep.write_text("x")
    assert _run(_script(tmp_path), hosts, rig).returncode == 0
    assert not stale.exists() and keep.exists()


def test_a_symlinked_or_relative_hosts_path_is_refused(tmp_path):
    rig = Rig(tmp_path)
    script = _script(tmp_path)
    real = _hosts(tmp_path)
    link = tmp_path / "hosts-link"
    link.symlink_to(real)
    r = _run(script, link, rig)
    assert r.returncode == 1 and "symlink" in r.stderr and link.is_symlink()
    r = subprocess.run(["bash", str(script), "hosts"], capture_output=True, text=True, cwd=tmp_path,
                       env=dict(os.environ, **rig.env()), timeout=60)
    assert r.returncode == 1 and "absolute" in r.stderr
    assert real.read_text() == "127.0.0.1 localhost\n10.77.9.1 gw\n"


def test_the_written_address_is_a_normalised_dotted_quad(tmp_path):
    r = _pick("010.077.009.195 STREAM dev1.local\n")
    assert r.returncode == 0 and r.stdout == "10.77.9.195\n"


def test_the_current_address_is_kept_while_dev1_still_announces_it(tmp_path):
    answer = "10.77.9.231 STREAM dev1.local\n10.77.9.195 STREAM\n"
    r = _bash("obs_box_lease_host_pick 10.77.9.204 23 10.77.9.195", stdin=answer)
    assert r.stdout == "10.77.9.195\n"
    r = _bash("obs_box_lease_host_pick 10.77.9.204 23 10.77.9.7", stdin=answer)
    assert r.stdout == "10.77.9.231\n"
    rig = Rig(tmp_path, ahosts=answer)
    hosts = _hosts(tmp_path, "127.0.0.1 localhost\n" + STOPGAP)
    ino = hosts.stat().st_ino
    assert _run(_script(tmp_path), hosts, rig).returncode == 0
    _unchanged(hosts, "127.0.0.1 localhost\n" + STOPGAP, ino)


def test_a_crlf_foreign_dev1_line_is_named():
    r = _bash("obs_box_lease_host_foreign", stdin="10.77.9.109 dev1\r\n127.0.0.1 localhost\r\n")
    assert r.stdout.splitlines() == ["10.77.9.109 dev1"]  # text=True folds the \r\n


def test_the_service_is_sandboxed_but_can_write_etc():
    svc = _bash("obs_box_lease_host_service_text").stdout
    for line in ("ProtectSystem=full", "ReadWritePaths=/etc", "NoNewPrivileges=yes", "PrivateTmp=yes",
                 "ProtectHome=yes"):
        assert "\n%s\n" % line in svc, line


# --- the install (setup-strih step 16l) ---------------------------------------------------------------
def test_install_adds_nss_mdns_when_missing_and_fails_when_apt_fails(tmp_path):
    a = tmp_path / "a"
    a.mkdir()
    rig = Rig(a, nss_installed=False)
    r = _bash("obs_box_lease_host_install", env=rig.env())
    assert r.returncode == 0 and "apt-get install -y libnss-mdns" in rig.calls(), r.stderr
    b = tmp_path / "b"
    b.mkdir()
    rig = Rig(b, nss_installed=False, apt_rc=100)
    r = _bash("obs_box_lease_host_install || echo RC=$?", env=rig.env())
    assert "RC=1" in r.stdout and "libnss-mdns failed" in r.stderr
    assert not list(rig.units.iterdir()) and "systemctl" not in rig.calls()


def test_install_writes_the_files_enables_the_timer_and_refreshes_once(tmp_path):
    rig = Rig(tmp_path)
    r = _bash("obs_box_lease_host_install", env=rig.env())
    assert r.returncode == 0, r.stderr
    script = rig.sbin / "camera-box-lease-host-refresh"
    assert os.access(script, os.X_OK)
    assert script.read_text() == _bash("obs_box_lease_host_script_text").stdout
    svc = (rig.units / "camera-box-lease-host.service").read_text()
    tmr = (rig.units / "camera-box-lease-host.timer").read_text()
    assert "Type=oneshot" in svc and "ExecStart=/usr/local/sbin/camera-box-lease-host-refresh" in svc
    assert "After=network-online.target avahi-daemon.service" in svc and "Wants=network-online.target" in svc
    assert "OnBootSec=30s" in tmr and "OnUnitActiveSec=5min" in tmr and "WantedBy=timers.target" in tmr
    calls = rig.calls()
    assert calls.index("systemctl daemon-reload") < calls.index("systemctl enable --now camera-box-lease-host.timer")
    assert calls.index("systemctl enable --now camera-box-lease-host.timer") < calls.index(
        "systemctl start camera-box-lease-host.service")


def test_a_rerun_is_quiet_and_a_failed_first_refresh_only_warns(tmp_path):
    rig = Rig(tmp_path)
    assert _bash("obs_box_lease_host_install", env=rig.env()).returncode == 0
    rig.log.unlink()
    (tmp_path / "start-fails").touch()
    r = _bash("obs_box_lease_host_install", env=rig.env())
    assert r.returncode == 0, r.stderr
    assert "daemon-reload" not in rig.calls() and r.stdout.count("unchanged") == 3
    assert "WARNING: the first camera-box-lease-host refresh failed" in r.stderr


def test_install_skips_a_box_outside_the_gate(tmp_path):
    rig = Rig(tmp_path)
    r = _bash("obs_box_lease_host_install", box="stream-pp", env=rig.env())
    assert r.returncode == 0 and "SKIP" in r.stdout and rig.calls() == ""
    assert not list(rig.units.iterdir())


# --- the grade (verify-strih) ---------------------------------------------------------------------------
def _grade(rig, box="stream-snv"):
    r = _bash("obs_box_lease_host_grade", box=box, env=rig.env())
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def test_grade_passes_a_provisioned_box(tmp_path):
    rig = Rig(tmp_path)
    assert _bash("obs_box_lease_host_install", env=rig.env()).returncode == 0
    out = _grade(rig)
    assert [ln.split()[0] for ln in out] == ["OK", "OK", "OK"], out
    assert "-m 5" in rig.calls() and "http://dev1:8890/rig-lease.json" in rig.calls()


def test_grade_fails_each_row_by_name(tmp_path):
    rig = Rig(tmp_path, hosts_answer="10.77.9.109     dev1\n", code="000", timer="enabled/inactive")
    out = _grade(rig)
    assert [ln.split()[0] for ln in out] == ["BAD", "BAD", "BAD"], out
    assert "(lease-host-unit)" in out[0] and "camera-box-lease-host-refresh" in out[0]
    assert "(lease-host)" in out[1] and "'10.77.9.109'" in out[1] and "'10.77.9.195'" in out[1]
    assert "(lease-http)" in out[2] and "'000'" in out[2]


def test_grade_fails_an_unresolved_dev1(tmp_path):
    rig = Rig(tmp_path, ahosts="", hosts_answer="")
    out = _grade(rig)
    assert out[1].startswith("BAD (lease-host) dev1 -> ''")


def test_grade_is_a_note_outside_the_gate(tmp_path):
    rig = Rig(tmp_path)
    out = _grade(rig, box="strih-snv")
    assert out == [out[0]] and out[0].startswith("NOTE (lease-host) not graded")


# --- the wiring + the line budgets -------------------------------------------------------------------
def test_setup_and_verify_wire_the_lease_host_within_their_budgets():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert 'step "16l"' in s and "obs_box_lease_host_install || fail" in s
    assert s.index("obs_box_step_applies 16j-restreamer") < s.index('step "16l"') < s.index("step 17 ")
    assert "\nobs_box_lease_host_grade " in v
    assert len(s.splitlines()) <= 1000 and len(v.splitlines()) <= 1000
