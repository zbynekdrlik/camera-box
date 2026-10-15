"""Issue 1429 -- the dantesync tray icon on every Linux workstation box (strih-snv, strih-pp, stream-pp).

WHY (owner, 14.10.2026): "dantesinc ma mat pod linuxom tiez v liste ikonuzo stavmi tak ako na windovse aby
sme videli". dantesync 1.18.0 (dantesync issue 132) ships `dantesync-tray-linux-amd64`; nothing on the Linux
boxes installed or started it.

What this pins (design comment 6176755827, Approach 1):
  * scripts/lib/dantesync-install.sh `dantesync_install_tray`: the PINNED release's tray + `.sha256`, the
    sha256 verified before anything is installed, `install -m 0755` to DANTESYNC_INSTALL_TRAY_BIN; an
    installed tray with the release's sha256 is kept, any other is replaced (the tray follows the pin);
    a pin below 1.18.0 is a named WARN + rc 0; a download or sha failure is rc 1 + a stderr line;
  * scripts/lib/strih-session-apps.sh: the fifth session app `dantesync-tray.service` (the unit text, the
    autostart line, the item label, the program seam, the packages, the install with its try-restart only
    when the unit or the binary changed, the grader row incl. the NOTE below 1.18.0);
  * the stream role keeps the tray (scripts/lib/obs-box-role.sh).

Tier-0: bash + pytest only. The release is a file:// tree the real curl reads; no network, no root.
"""
import hashlib
import os
import shutil
import stat
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import strih_session_apps_fakes_1399 as F  # noqa: E402

REPO = F.REPO
DS_INSTALL = F.SCRIPTS / "lib" / "dantesync-install.sh"
TRAY_UNIT_FILE = REPO / "systemd" / "dantesync-tray.service"
TRAY = "dantesync-tray.service"
GOOD = b"#!/bin/sh\n# dantesync tray v1.18.0\n"


def _tray(tmp_path, env=None, release=None):
    """Run dantesync_install_tray (its own lib only) under the caller's set -euo pipefail."""
    e = F.tray_env(tmp_path, **(release or {}))
    e.update(env or {})
    return F.bash('dantesync_install_tray && echo "rc=0" || echo "rc=$?"', env=e, sources=(DS_INSTALL,))


def _rc(r):
    assert r.stdout.splitlines(), r.stderr
    return int(r.stdout.splitlines()[-1].split("=", 1)[1])


# --- the version compare + the pin ------------------------------------------------------------------------

@pytest.mark.parametrize("a,b,rc", [
    ("1.18.0", "1.18.0", 0), ("1.18.1", "1.18.0", 0), ("1.19.0", "1.18.0", 0), ("2.0.0", "1.18.0", 0),
    ("1.17.9", "1.18.0", 1), ("1.17.0", "1.18.0", 1), ("0.99.99", "1.18.0", 1),
    ("1.100.0", "1.18.0", 0),  # numeric, never text order ("100" < "18" as text)
    ("1.9.0", "1.18.0", 1),
    ("1.18", "1.18.0", 2), ("v1.18.0", "1.18.0", 2), ("", "1.18.0", 2),
])
def test_version_ge(a, b, rc):
    r = F.bash('dantesync_install_version_ge "%s" "%s" && echo rc=0 || echo "rc=$?"' % (a, b), sources=(DS_INSTALL,))
    assert _rc(r) == rc, r.stderr


def test_the_tray_follows_the_one_pin_and_its_release_asset():
    r = F.bash('dantesync_install_tray_url 1.18.0; echo; printf "%s" "$DANTESYNC_TRAY_FIRST_VERSION"',
               sources=(DS_INSTALL,), env={"DANTESYNC_RELEASE_BASE": "https://github.com/zbynekdrlik/dantesync/releases/download"})
    assert r.stdout.split("\n") == [
        "https://github.com/zbynekdrlik/dantesync/releases/download/v1.18.0/dantesync-tray-linux-amd64", "1.18.0"]
    r = F.bash("dantesync_install_tray_bin", sources=(DS_INSTALL,), env={"DANTESYNC_INSTALL_TRAY_BIN": ""})
    assert r.stdout == "/usr/local/bin/dantesync-tray"


# --- dantesync_install_tray ----------------------------------------------------------------------------------

def test_a_pin_below_1_18_0_warns_and_installs_nothing(tmp_path):
    r = _tray(tmp_path, env={"DANTESYNC_VERSION_PIN": "1.17.0"})
    assert _rc(r) == 0, r.stderr
    assert "WARN: dantesync v1.17.0 ships no Linux tray" in r.stdout and "1.18.0" in r.stdout
    assert not F.tray_bin(tmp_path).exists()


def test_a_rollback_below_1_18_0_removes_the_tray_left_from_the_newer_pin(tmp_path):
    # review round 2: else verify reads stale forever and step 16d (which only WARNs there) cannot clear it
    F.fake_tray(tmp_path)
    r = _tray(tmp_path, env={"DANTESYNC_VERSION_PIN": "1.17.0"})
    assert _rc(r) == 0, r.stderr
    assert not F.tray_bin(tmp_path).exists() and not F.tray_record(tmp_path).exists()
    assert "removed the dantesync tray" in r.stdout and "rollback to v1.17.0" in r.stdout


def test_a_pin_below_1_18_0_never_downloads(tmp_path):
    # the release base does not exist: a download attempt would fail rc 1
    r = F.bash('dantesync_install_tray && echo "rc=0" || echo "rc=$?"', sources=(DS_INSTALL,),
               env={"DANTESYNC_VERSION_PIN": "1.17.9", "DANTESYNC_INSTALL_TRAY_BIN": str(F.tray_bin(tmp_path))})
    assert _rc(r) == 0 and r.stderr == "", r.stderr


def test_the_gate_pin_is_read_when_no_override_is_set(tmp_path):
    gate = (F.SCRIPTS / "dantesync-version-gate.sh").read_text()
    pin = gate.split('DANTESYNC_VERSION_PIN="${DANTESYNC_VERSION_PIN:-', 1)[1].split("}", 1)[0]
    r = F.bash('dantesync_install_tray && echo "rc=0" || echo "rc=$?"', sources=(DS_INSTALL,),
               env={"DANTESYNC_INSTALL_TRAY_BIN": str(F.tray_bin(tmp_path)),
                    "DANTESYNC_INSTALL_TRAY_RELEASE_FILE": str(F.tray_record(tmp_path)),
                    "DANTESYNC_RELEASE_BASE": F.fake_tray_release(tmp_path, version=pin)})
    assert _rc(r) == 0, r.stderr
    below = subprocess.run(["bash", "-c", '. "%s"; dantesync_install_version_ge "%s" 1.18.0' % (DS_INSTALL, pin)]).returncode
    assert ("ships no Linux tray" in r.stdout) == (below != 0), r.stdout


def test_a_good_download_installs_the_tray(tmp_path):
    r = _tray(tmp_path)
    assert _rc(r) == 0, r.stderr
    b = F.tray_bin(tmp_path)
    assert b.read_bytes() == GOOD
    assert stat.S_IMODE(b.stat().st_mode) == 0o755
    assert "installed the dantesync tray v1.18.0 -> %s" % b in r.stdout
    # the release record verify grades with no network read
    assert F.tray_record(tmp_path).read_text() == "v1.18.0 %s\n" % hashlib.sha256(GOOD).hexdigest()
    assert sorted(p.name for p in b.parent.iterdir()) == ["dantesync-tray"], "no .new sibling left behind"


def test_an_upper_case_release_sha256_is_accepted(tmp_path):
    r = _tray(tmp_path, release={"sha": hashlib.sha256(GOOD).hexdigest().upper()})
    assert _rc(r) == 0, r.stderr
    assert F.tray_bin(tmp_path).read_bytes() == GOOD


def test_a_replace_renames_a_new_inode_over_the_name(tmp_path):
    # a running tray keeps the inode it runs; the new one arrives by rename, never a write into the old file
    b = F.fake_tray(tmp_path)
    old_ino = b.stat().st_ino
    keep = b.parent / "held"
    os.link(b, keep)  # stands in for the running process holding the old inode
    r = _tray(tmp_path)
    assert _rc(r) == 0, r.stderr
    assert b.stat().st_ino != old_ino and keep.read_text() == "#!/bin/sh\n"
    assert not [p for p in b.parent.iterdir() if ".new." in p.name]


def test_a_kept_tray_gets_its_missing_release_record_back(tmp_path):
    assert _rc(_tray(tmp_path)) == 0
    F.tray_record(tmp_path).unlink()
    r = _tray(tmp_path)
    assert _rc(r) == 0 and "-- kept" in r.stdout, r.stderr
    assert F.tray_record(tmp_path).read_text().startswith("v1.18.0 ")


def test_a_sha_mismatch_refuses_and_installs_nothing(tmp_path):
    r = _tray(tmp_path, release={"sha": "0" * 64})
    assert _rc(r) == 1
    assert "SHA256 MISMATCH for the dantesync v1.18.0 tray" in r.stderr and "refusing" in r.stderr
    assert not F.tray_bin(tmp_path).exists()


def test_a_sha_mismatch_never_touches_an_installed_tray(tmp_path):
    b = F.fake_tray(tmp_path)
    before = (b.read_bytes(), b.stat().st_mtime_ns)
    r = _tray(tmp_path, release={"sha": "0" * 64})
    assert _rc(r) == 1
    assert (b.read_bytes(), b.stat().st_mtime_ns) == before


def test_a_malformed_sha256_file_refuses(tmp_path):
    r = _tray(tmp_path, release={"sha": "not-a-sha"})
    assert _rc(r) == 1 and "is not a sha256" in r.stderr
    assert not F.tray_bin(tmp_path).exists()


def test_a_failed_download_is_rc_1_with_a_stderr_line(tmp_path):
    r = F.bash('dantesync_install_tray && echo "rc=0" || echo "rc=$?"', sources=(DS_INSTALL,),
               env={"DANTESYNC_VERSION_PIN": "1.18.0", "DANTESYNC_INSTALL_TRAY_BIN": str(F.tray_bin(tmp_path)),
                    "DANTESYNC_RELEASE_BASE": "file://%s/no-such-release" % tmp_path})
    assert _rc(r) == 1
    assert "download of the dantesync v1.18.0 tray" in r.stderr
    assert not F.tray_bin(tmp_path).exists()


def test_the_same_sha_keeps_the_installed_tray(tmp_path):
    assert _rc(_tray(tmp_path)) == 0
    b = F.tray_bin(tmp_path)
    past = time.time() - 3600
    os.utime(b, (past, past))
    before = b.stat().st_mtime_ns
    r = _tray(tmp_path)
    assert _rc(r) == 0, r.stderr
    assert b.stat().st_mtime_ns == before, "an unchanged tray keeps its mtime (verify's process check reads it)"
    assert "sha256 matches the release) -- kept" in r.stdout


def test_a_different_sha_replaces_the_installed_tray(tmp_path):
    b = F.fake_tray(tmp_path)
    old_sha = hashlib.sha256(b.read_bytes()).hexdigest()
    r = _tray(tmp_path)
    assert _rc(r) == 0, r.stderr
    assert b.read_bytes() == GOOD and stat.S_IMODE(b.stat().st_mode) == 0o755
    assert "replaced the dantesync tray with v1.18.0" in r.stdout and old_sha[:12] in r.stdout


def test_a_non_executable_tray_with_the_right_bytes_is_reinstalled(tmp_path):
    b = F.tray_bin(tmp_path)
    b.parent.mkdir(parents=True)
    b.write_bytes(GOOD)
    b.chmod(0o644)
    r = _tray(tmp_path)
    assert _rc(r) == 0, r.stderr
    assert stat.S_IMODE(b.stat().st_mode) == 0o755


# --- dantesync_install_tray_state: the release record, no network ---------------------------------------------

def _state(tmp_path, pin="1.18.0"):
    env = {"DANTESYNC_INSTALL_TRAY_BIN": str(F.tray_bin(tmp_path)),
           "DANTESYNC_INSTALL_TRAY_RELEASE_FILE": str(F.tray_record(tmp_path)), "DANTESYNC_VERSION_PIN": pin}
    r = F.bash('dantesync_install_tray_state && echo " rc=0" || echo " rc=$?"', env=env, sources=(DS_INSTALL,))
    return r.stdout.strip()


def test_state_ok_after_an_install(tmp_path):
    assert _rc(_tray(tmp_path)) == 0
    assert _state(tmp_path) == "ok rc=0"


def test_state_absent(tmp_path):
    assert _state(tmp_path) == "absent rc=1"


def test_state_stale_when_the_pin_moved_on(tmp_path):
    assert _rc(_tray(tmp_path)) == 0
    assert _state(tmp_path, pin="1.19.0") == "stale:installed-v1.18.0-pin-v1.19.0 rc=1"


def test_state_stale_when_the_binary_is_not_the_recorded_release(tmp_path):
    assert _rc(_tray(tmp_path)) == 0
    F.tray_bin(tmp_path).write_text("#!/bin/sh\n# a hand copy\n")
    assert _state(tmp_path) == "stale:sha256-is-not-the-v1.18.0-release rc=1"


def test_state_stale_without_a_release_record(tmp_path):
    assert _rc(_tray(tmp_path)) == 0
    F.tray_record(tmp_path).unlink()
    assert _state(tmp_path) == "stale:no-release-record rc=1"


# --- the session app: unit, autostart, mapping, packages --------------------------------------------------------

def _unit():
    return {line.split("=", 1)[0]: line.split("=", 1)[1]
            for line in TRAY_UNIT_FILE.read_text().splitlines() if "=" in line and not line.startswith("#")}


def test_the_unit_text():
    u = _unit()
    assert u["ExecStart"] == "/usr/local/bin/dantesync-tray"
    assert u["Restart"] == "always" and u["RestartSec"] == "5" and u["StartLimitIntervalSec"] == "0"
    assert u["Type"] == "simple" and u["Environment"] == "DISPLAY=:0"
    assert u["StandardOutput"] == "journal" and u["StandardError"] == "journal"
    assert u["WantedBy"] == "graphical-session.target"  # the strih-obs pattern, never default.target
    # no executable binary (a pin below 1.18.0) = a clean condition skip, never a restart loop on a
    # missing ExecStart; executable, not just present, the way the grader reads it (review round 1)
    assert u["ConditionFileIsExecutable"] == u["ExecStart"]
    assert "ConditionPathExists" not in u


def test_the_unit_runs_the_binary_the_install_writes():
    r = F.bash("dantesync_install_tray_bin", sources=(DS_INSTALL,), env={"DANTESYNC_INSTALL_TRAY_BIN": ""})
    assert _unit()["ExecStart"] == r.stdout
    r = F.bash("strih_session_app_program %s" % TRAY, env={"DANTESYNC_INSTALL_TRAY_BIN": ""})
    assert r.returncode == 0 and r.stdout == "/usr/local/bin/dantesync-tray", r.stderr
    r = F.bash("strih_session_app_program %s" % TRAY, env={"DANTESYNC_INSTALL_TRAY_BIN": "/tmp/x-tray"})
    assert r.stdout == "/tmp/x-tray", "ONE seam: the install and the grader read the same path"


def test_the_tray_is_the_fifth_session_app_with_its_autostart_line():
    r = F.bash('printf "%s\\n" "${STRIH_SESSION_APP_UNITS[@]}"')
    assert r.stdout.split() == list(F.UNITS) and r.stdout.split()[-1] == TRAY
    r = F.bash("strih_session_app_autostart_line %s" % TRAY)
    assert r.stdout == "systemctl --user start dantesync-tray.service || true"
    assert r.stdout in F.bash("strih_session_apps_autostart_lines").stdout.splitlines()
    r = F.bash('printf "[%%s] %%s" "$(strih_session_app_script %s)" "$(strih_session_app_item %s)"' % (TRAY, TRAY))
    assert r.stdout == "[] dantesync-tray"


def test_the_runtime_packages_join_the_session_app_packages():
    r = F.bash('printf "%s\\n" "${STRIH_SESSION_APP_PACKAGES[@]}"')
    for pkg in ("libgtk-3-0t64", "libayatana-appindicator3-1", "libxdo3"):
        assert pkg in r.stdout.split(), r.stdout


def test_the_kiosk_autostart_starts_the_tray_after_obs():
    r = F.bash("strih_openbox_autostart_text", sources=(F.SCRIPTS / "lib" / "obs-box-baseline.sh",
                                                         F.SCRIPTS / "lib" / "strih-provision.sh"))
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert lines.index("systemctl --user start strih-obs.service || true") < lines.index(
        "systemctl --user start dantesync-tray.service || true")


# --- the install (setup-strih step 16d) -------------------------------------------------------------------------

def _install(tmp_path, home, env_extra=None, **fakes):
    if (tmp_path / "bin").exists():
        shutil.rmtree(tmp_path / "bin")
    b, log = F.fake_bin(tmp_path, **fakes)
    py = F.fake_python_ok(tmp_path)
    env = {"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "usrlocalbin"), "STRIH_SESSION_APPS_EUID": "1000",
           "STRIH_SESSION_APPS_PYTHON": str(py)}
    env.update(F.tray_env(tmp_path))
    env.update(env_extra or {})
    r = F.bash('strih_session_apps_install "%s" "%s" newlevel' % (REPO, home), env=env, path_prepend=str(b))
    return r, log.read_text() if log.exists() else ""


def _restarted(calls):
    lines = [ln for ln in calls.splitlines() if ln.startswith("systemctl --user try-restart ")]
    return lines[0].split()[3:] if lines else []


def test_install_fetches_the_tray_writes_its_unit_and_enables_it(tmp_path):
    home = tmp_path / "home"
    r, calls = _install(tmp_path, home)
    assert r.returncode == 0, r.stdout + r.stderr
    assert F.tray_bin(tmp_path).read_bytes() == GOOD
    unit = home / ".config/systemd/user" / TRAY
    assert unit.read_bytes() == TRAY_UNIT_FILE.read_bytes()
    assert stat.S_IMODE(unit.stat().st_mode) == 0o644
    assert any(ln.startswith("systemctl --user enable ") and TRAY in ln.split() for ln in calls.splitlines())
    assert TRAY in _restarted(calls)
    for ln in calls.splitlines():
        if ln.startswith("systemctl"):
            assert "start" not in ln.split() and "restart" not in ln.split() and "--now" not in ln.split(), ln
    for pkg in ("libgtk-3-0t64", "libayatana-appindicator3-1", "libxdo3"):
        assert ("dpkg-query -W -f=${Status} %s" % pkg) in calls


def test_install_restarts_the_tray_only_when_its_binary_changed(tmp_path):
    home = tmp_path / "home"
    assert _install(tmp_path, home)[0].returncode == 0
    r, calls = _install(tmp_path, home)
    assert r.returncode == 0, r.stderr
    assert "try-restart" not in calls, "an unchanged tray + unit restarts nothing"
    # the pin moved on: the release has another tray -> the binary is replaced and the running tray restarted
    r, calls = _install(tmp_path, home, env_extra={
        "DANTESYNC_RELEASE_BASE": F.fake_tray_release(tmp_path / "next", content=b"#!/bin/sh\n# newer tray\n")})
    assert r.returncode == 0, r.stderr
    assert _restarted(calls) == [TRAY]
    assert "daemon-reload" not in calls, "no unit file changed: nothing to reload"
    assert "replaced the dantesync tray with v1.18.0" in r.stdout
    assert "installed %s" % TRAY in r.stdout and "binary written, unit unchanged" in r.stdout


def test_install_below_1_18_0_warns_and_still_provisions_the_unit(tmp_path):
    home = tmp_path / "home"
    r, calls = _install(tmp_path, home, env_extra={"DANTESYNC_VERSION_PIN": "1.17.0"})
    assert r.returncode == 0, r.stderr
    assert "WARN: dantesync v1.17.0 ships no Linux tray" in r.stdout
    assert not F.tray_bin(tmp_path).exists()
    assert (home / ".config/systemd/user" / TRAY).is_file()
    assert any(ln.startswith("systemctl --user enable ") and TRAY in ln.split() for ln in calls.splitlines())
    assert r.stdout.count("ships no Linux tray") == 1, "one WARN, not a second generic one from the loop"


def test_install_a_failed_tray_fetch_warns_and_provisions_every_other_app(tmp_path):
    home = tmp_path / "home"
    r, calls = _install(tmp_path, home, env_extra={"DANTESYNC_RELEASE_BASE": F.fake_tray_release(
        tmp_path / "bad", sha="0" * 64)})
    assert r.returncode == 0, r.stdout + r.stderr
    assert "SHA256 MISMATCH" in r.stderr
    assert "WARN: the dantesync tray step failed" in r.stdout and "step 16d" in r.stdout
    tray_warns = [ln for ln in r.stdout.splitlines()
                  if "WARN" in ln and ("dantesync tray" in ln or str(F.tray_bin(tmp_path)) in ln)]
    assert len(tray_warns) == 1, "one WARN for one failed fetch (review round 1): %s" % tray_warns
    assert not F.tray_bin(tmp_path).exists()
    for unit in F.UNIT_FILES:
        assert (home / ".config/systemd/user" / unit).is_file(), unit


# --- the grader row (verify-strih) --------------------------------------------------------------------------------

def _rows(tmp_path, env_extra=None, home_kw=None, **fakes):
    home, bindir = F.installed_home(tmp_path, **(home_kw or {}))
    F.all_states(tmp_path / "run")
    if (tmp_path / "bin").exists():
        shutil.rmtree(tmp_path / "bin")
    b, _log = F.fake_bin(tmp_path, wmctrl=F.WIN, **fakes)
    env = F.grade_env(tmp_path, bindir, tmp_path / "run")
    env.update(env_extra or {})
    r = F.bash('strih_session_apps_grade_rows "%s" "%s" newlevel' % (REPO, home), env=env, path_prepend=str(b))
    assert r.returncode == 0 and r.stderr == "", r.stderr
    return r.stdout.splitlines()


def test_grade_row_ok(tmp_path):
    rows = _rows(tmp_path)
    b = F.tray_bin(tmp_path)
    assert F.row(rows, "dantesync-tray") == (
        "OK|(dantesync-tray) %s: unit matches the checkout, %s installed, enabled, started by the kiosk "
        "autostart, active, its process runs %s and started after the unit was installed" % (TRAY, b, b))
    assert len(rows) == len(F.UNITS) + 3


def test_grade_row_note_while_the_pin_has_no_linux_tray(tmp_path):
    rows = _rows(tmp_path, env_extra={"DANTESYNC_VERSION_PIN": "1.17.0"}, home_kw={"tray": False})
    row = F.row(rows, "dantesync-tray")
    assert row.startswith("NOTE|(dantesync-tray) %s: " % TRAY)
    assert "v1.17.0" in row and "1.18.0" in row and "DANTESYNC_VERSION_PIN" in row


def test_grade_row_fails_a_missing_tray_once_the_pin_ships_one(tmp_path):
    rows = _rows(tmp_path, home_kw={"tray": False})
    row = F.row(rows, "dantesync-tray")
    assert row.startswith("FAIL|(dantesync-tray) %s: script-absent" % TRAY)
    assert "step 16d" in row and "dantesync" in row.split(" -- ", 1)[1]


def test_grade_row_fails_a_tray_behind_the_pin(tmp_path):
    # the pin moved to 1.19.0 and setup-strih did not follow (or its fetch failed): a FAIL, never a quiet OK
    rows = _rows(tmp_path, env_extra={"DANTESYNC_VERSION_PIN": "1.19.0"})
    row = F.row(rows, "dantesync-tray")
    assert row.startswith("FAIL|(dantesync-tray) %s: script-differs" % TRAY), row
    assert "program=stale:installed-v1.18.0-pin-v1.19.0" in row and "step 16d" in row


def test_grade_row_fails_a_hand_replaced_tray(tmp_path):
    home_rows = _rows(tmp_path)
    assert F.row(home_rows, "dantesync-tray").startswith("OK|"), home_rows
    F.tray_bin(tmp_path).write_text("#!/bin/sh\n# a hand copy\n")
    rows = _rows(tmp_path / "again", env_extra={
        "DANTESYNC_INSTALL_TRAY_BIN": str(F.tray_bin(tmp_path)),
        "DANTESYNC_INSTALL_TRAY_RELEASE_FILE": str(F.tray_record(tmp_path))},
        home_kw={"tray_exe": str(F.tray_bin(tmp_path))})
    assert "program=stale:sha256-is-not-the-v1.18.0-release" in F.row(rows, "dantesync-tray")


def test_grade_row_note_still_fails_a_broken_unit_install(tmp_path):
    # below 1.18.0 a missing binary is a NOTE only when the unit, its enablement and autostart are in place
    rows = _rows(tmp_path, env_extra={"DANTESYNC_VERSION_PIN": "1.17.0"},
                 home_kw={"tray": False, "enabled": False})
    row = F.row(rows, "dantesync-tray")
    assert row.startswith("FAIL|(dantesync-tray) %s: not-enabled" % TRAY), row
    assert "ships no Linux tray" in row


def test_grade_row_an_installed_tray_is_graded_in_full(tmp_path):
    rows = _rows(tmp_path, active="inactive")
    assert F.row(rows, "dantesync-tray").startswith("FAIL|(dantesync-tray) %s: not-active:inactive" % TRAY)


def test_grade_row_an_installed_tray_below_1_18_0_is_never_a_note(tmp_path):
    # a binary on the box is graded whatever the pin says: the NOTE is only for "nothing to run yet". A pin
    # rolled back below 1.18.0 with a tray left installed reads stale (its record names another release).
    rows = _rows(tmp_path, env_extra={"DANTESYNC_VERSION_PIN": "1.17.0"})
    row = F.row(rows, "dantesync-tray")
    assert row.startswith("FAIL|(dantesync-tray) %s: script-differs" % TRAY), row
    assert "program=stale:installed-v1.18.0-pin-v1.17.0" in row


def test_grade_row_a_tray_started_some_other_way_is_the_wrong_program(tmp_path):
    rows = _rows(tmp_path, home_kw={"tray_exe": "/usr/bin/python3"})
    assert F.row(rows, "dantesync-tray").startswith("FAIL|(dantesync-tray) %s: process:wrong-program" % TRAY)


def test_grade_row_a_replaced_tray_under_the_running_process_is_the_wrong_program(tmp_path):
    rows = _rows(tmp_path, home_kw={"tray_exe": "%s (deleted)" % F.tray_bin(tmp_path)})
    assert F.row(rows, "dantesync-tray").startswith("FAIL|(dantesync-tray) %s: process:wrong-program" % TRAY)


def test_grade_row_a_tray_older_than_its_files_is_stale(tmp_path):
    rows = _rows(tmp_path, started=int(time.time()) - 3 * 86400)
    row = F.row(rows, "dantesync-tray")
    assert row.startswith("FAIL|(dantesync-tray) %s: process:stale" % TRAY)
    assert "systemctl --user restart %s" % TRAY in row


def test_grade_row_not_enabled(tmp_path):
    rows = _rows(tmp_path, home_kw={"enabled": False})
    assert F.row(rows, "dantesync-tray").startswith("FAIL|(dantesync-tray) %s: not-enabled" % TRAY)


def test_grade_report_routes_a_note_row_to_note(tmp_path):
    home, bindir = F.installed_home(tmp_path, tray=False)
    F.all_states(tmp_path / "run")
    b, _ = F.fake_bin(tmp_path, wmctrl=F.WIN)
    env = F.grade_env(tmp_path, bindir, tmp_path / "run")
    env["DANTESYNC_VERSION_PIN"] = "1.17.0"
    r = F.bash('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }; note() { echo "NOTE $1"; }; '
               'strih_session_apps_grade_report "%s" "%s" newlevel; echo rc=$?' % (REPO, home),
               env=env, path_prepend=str(b))
    assert r.returncode == 0, r.stderr
    assert "NOTE (dantesync-tray) " in r.stdout and "FAIL " not in r.stdout
    assert r.stdout.strip().endswith("rc=0"), "a NOTE row still counts toward the row total"
