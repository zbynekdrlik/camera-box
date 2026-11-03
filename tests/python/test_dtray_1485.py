"""Issue 1485 -- dstack's shared tray `dtray` on every Linux OBS workstation (the dsync tray's set).

What this pins (design comment 6609295433, Approach 1):
  * scripts/dtray-release.pin: the tag, the asset, its sha256 and the first dstack version dtray can see;
  * scripts/lib/dtray.sh `dtray_install`: the pinned asset only, its release `.sha256` must name the pin,
    the download proved against the pin before install, kept when already the pin, renamed into place,
    a release record for a network-free verify; the XDG autostart entry removed;
  * `dtray_state` (absent / stale:<why> / ok), the expected-device markers (`/etc/dstack/devices`, ours
    recorded, a foreign one never touched), the `dtray --print` grade (a missing device is a NOTE while
    the pinned intercom or obs dstack is below the first dtray sees, a FAIL after; the threshold read
    from the pin files);
  * scripts/lib/strih-session-apps.sh: the seventh session app `dtray.service` (both roles), its unit,
    the install through the loop and the grader rows.

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
DTRAY_LIB = F.SCRIPTS / "lib" / "dtray.sh"
UNIT_FILE = REPO / "systemd" / "dtray.service"
UNIT = "dtray.service"
PIN = REPO / "scripts" / "dtray-release.pin"


def _release(tmp_path, content=F.DTRAY_GOOD, sha=None, tag=F.DTRAY_TAG, asset=F.DTRAY_ASSET):
    """A dstack release tree over file:// with the asset + its `.sha256` (`sha` replaces the true hash)."""
    d = tmp_path / "release" / tag
    d.mkdir(parents=True, exist_ok=True)
    (d / asset).write_bytes(content)
    (d / (asset + ".sha256")).write_text("%s  %s\n" % (sha or hashlib.sha256(content).hexdigest(), asset))
    return "file://%s" % (tmp_path / "release")


def _env(tmp_path, **kw):
    e = F.dtray_env(tmp_path)
    e["DTRAY_RELEASE_BASE"] = _release(tmp_path, **kw)
    return e


def _dt(body, env=None, path_prepend=None):
    return F.bash(body, env=env, sources=(DTRAY_LIB,), path_prepend=path_prepend)


def _rc(r):
    assert r.stdout.splitlines(), r.stderr
    return int(r.stdout.splitlines()[-1].rsplit("=", 1)[1])


def _install(tmp_path, home=None, env=None):
    e = _env(tmp_path)
    e.update(env or {})
    return _dt('dtray_install "%s" && echo "rc=0" || echo "rc=$?"' % (home or tmp_path / "home"), env=e)


# --- the pin -------------------------------------------------------------------------------------------

def test_the_shipped_pin_names_the_dstack_release():
    pin = dict(ln.split("=", 1) for ln in PIN.read_text().splitlines() if ln)
    assert pin == {"DTRAY_RELEASE_TAG": "v0.6.19", "DTRAY_RELEASE_ASSET": "dtray-v0.6.19-linux-x86_64",
                   "DTRAY_RELEASE_SHA256": "408d6cea7e70a4e682ba51776994dfa37d4db4a6fa78a97a1f94ce17121d88bd",
                   "DTRAY_DEVICE_FIRST_DSTACK": "v0.6.13"}
    r = _dt('dtray_release_url', env={"DTRAY_RELEASE_BASE": ""})
    assert r.returncode == 0, r.stderr
    assert r.stdout == "https://github.com/zbynekdrlik/dstack/releases/download/v0.6.19/dtray-v0.6.19-linux-x86_64"


def test_the_intercom_and_obs_pins_name_their_dstack_version():
    for pin, key in (("intercom-release.pin", "INTERCOM_RELEASE_DSTACK_VERSION"),
                     ("obs-release.pin", "OBS_RELEASE_DSTACK_VERSION")):
        vals = [ln.split("=", 1)[1] for ln in (REPO / "scripts" / pin).read_text().splitlines()
                if ln.startswith(key + "=")]
        assert len(vals) == 1 and vals[0].startswith("v") and vals[0].count(".") == 2, (pin, vals)


@pytest.mark.parametrize("line", ["DTRAY_RELEASE_SHA256=" + "A" * 64, "DTRAY_RELEASE_SHA256=abc",
                                  "DTRAY_RELEASE_SHA256="])
def test_a_malformed_pin_refuses(tmp_path, line):
    p = tmp_path / "pin"
    p.write_text("DTRAY_RELEASE_TAG=v0.6.19\nDTRAY_RELEASE_ASSET=a\n%s\n" % line)
    r = _dt('dtray_pin RELEASE_SHA256 && echo rc=0 || echo "rc=$?"', env={"DTRAY_PIN_FILE": str(p)})
    assert _rc(r) == 1 and "no valid DTRAY_RELEASE_SHA256" in r.stderr


# --- dtray_install -------------------------------------------------------------------------------------

def test_a_good_release_installs_the_binary_and_its_record(tmp_path):
    r = _install(tmp_path)
    assert _rc(r) == 0, r.stderr
    b = F.dtray_bin(tmp_path)
    assert b.read_bytes() == F.DTRAY_GOOD and stat.S_IMODE(b.stat().st_mode) == 0o755
    assert "installed dtray v0.6.19 -> %s" % b in r.stdout
    assert F.dtray_record(tmp_path).read_text() == "v0.6.19 %s\n" % hashlib.sha256(F.DTRAY_GOOD).hexdigest()
    assert sorted(p.name for p in b.parent.iterdir()) == ["dtray"], "no .new sibling left"


def test_a_release_whose_sha256_file_is_not_the_pin_downloads_nothing(tmp_path):
    # the release .sha256 names other bytes than the pin: refused before any download
    e = _env(tmp_path, content=b"#!/bin/sh\n# another build\n")
    r = _dt('dtray_install "%s" && echo rc=0 || echo "rc=$?"' % (tmp_path / "home"), env=e)
    assert _rc(r) == 1 and "the pin says" in r.stderr
    assert not F.dtray_bin(tmp_path).exists()


def test_a_download_that_is_not_the_pin_is_refused(tmp_path):
    # the .sha256 says the pin, the asset is other bytes
    e = _env(tmp_path, content=b"#!/bin/sh\n# tampered\n", sha=hashlib.sha256(F.DTRAY_GOOD).hexdigest())
    r = _dt('dtray_install "%s" && echo rc=0 || echo "rc=$?"' % (tmp_path / "home"), env=e)
    assert _rc(r) == 1 and "SHA256 MISMATCH" in r.stderr
    assert not F.dtray_bin(tmp_path).exists()


def test_a_failed_download_is_rc_1_and_never_touches_an_installed_dtray(tmp_path):
    b = F.fake_dtray(tmp_path, content=b"#!/bin/sh\n# old\n")
    before = (b.read_bytes(), b.stat().st_mtime_ns)
    e = F.dtray_env(tmp_path)
    e["DTRAY_RELEASE_BASE"] = "file://%s/no-such-release" % tmp_path
    r = _dt('dtray_install "%s" && echo rc=0 || echo "rc=$?"' % (tmp_path / "home"), env=e)
    assert _rc(r) == 1 and "download of the dtray v0.6.19 .sha256 failed" in r.stderr
    assert (b.read_bytes(), b.stat().st_mtime_ns) == before


def test_the_pinned_binary_is_kept_with_its_mtime(tmp_path):
    assert _rc(_install(tmp_path)) == 0
    b = F.dtray_bin(tmp_path)
    past = time.time() - 3600
    os.utime(b, (past, past))
    before = b.stat().st_mtime_ns
    F.dtray_record(tmp_path).unlink()
    r = _install(tmp_path)
    assert _rc(r) == 0 and "-- kept" in r.stdout, r.stderr
    assert b.stat().st_mtime_ns == before
    assert F.dtray_record(tmp_path).read_text().startswith("v0.6.19 "), "the record comes back"


def test_another_binary_is_replaced_by_a_rename(tmp_path):
    b = F.fake_dtray(tmp_path, content=b"#!/bin/sh\n# old\n")
    ino = b.stat().st_ino
    r = _install(tmp_path)
    assert _rc(r) == 0, r.stderr
    assert b.read_bytes() == F.DTRAY_GOOD and b.stat().st_ino != ino
    assert "replaced dtray with v0.6.19" in r.stdout


def test_the_xdg_autostart_entry_is_removed(tmp_path):
    home = tmp_path / "home"
    desk = home / ".config" / "autostart" / "dtray.desktop"
    desk.parent.mkdir(parents=True)
    desk.write_text("[Desktop Entry]\nExec=dtray\n")
    other = desk.parent / "other.desktop"
    other.write_text("x")
    r = _install(tmp_path, home=home)
    assert _rc(r) == 0, r.stderr
    assert not desk.exists() and other.exists()
    assert "removed ~/.config/autostart/dtray.desktop" in r.stdout


# --- dtray_state ---------------------------------------------------------------------------------------

def _state(tmp_path, env=None):
    e = F.dtray_env(tmp_path)
    e.update(env or {})
    return _dt('dtray_state && echo " rc=0" || echo " rc=$?"', env=e).stdout.strip()


def test_state_ok_absent_and_stale(tmp_path):
    assert _state(tmp_path) == "absent rc=1"
    F.fake_dtray(tmp_path)
    assert _state(tmp_path) == "ok rc=0"
    newer = F.dtray_pin(tmp_path / "p2", tag="v0.6.20")
    assert _state(tmp_path, {"DTRAY_PIN_FILE": str(newer)}) == "stale:installed-v0.6.19-pin-v0.6.20 rc=1"
    F.dtray_bin(tmp_path).write_text("#!/bin/sh\n# hand copy\n")
    assert _state(tmp_path) == "stale:sha256-is-not-the-v0.6.19-release rc=1"
    F.fake_dtray(tmp_path)
    F.dtray_record(tmp_path).unlink()
    assert _state(tmp_path) == "stale:no-release-record rc=1"


def test_state_stale_when_the_record_matches_but_the_pin_moved_its_sha(tmp_path):
    F.fake_dtray(tmp_path)
    other = F.dtray_pin(tmp_path / "p3", sha="b" * 64)
    assert _state(tmp_path, {"DTRAY_PIN_FILE": str(other)}) == "stale:sha256-is-not-the-v0.6.19-release rc=1"


# --- the expected-device markers ------------------------------------------------------------------------

SNV = "strih-snv-intercom=4441:8801:4456:47100-47131 strih-obs=4440:8800:4455:47200-47231"


def _apply(tmp_path, devices):
    e = F.dtray_env(tmp_path)
    e["DTRAY_DANTE_DEVICES"] = devices
    return _dt('dtray_devices_apply && echo rc=0 || echo "rc=$?"', env=e)


def test_markers_for_each_declared_device_and_ours_recorded(tmp_path):
    r = _apply(tmp_path, SNV)
    assert _rc(r) == 0, r.stderr
    d = tmp_path / "dstack-devices"
    assert sorted(p.name for p in d.iterdir()) == ["strih-obs.device", "strih-snv-intercom.device"]
    for p in d.iterdir():
        assert p.read_bytes() == b"" and stat.S_IMODE(p.stat().st_mode) == 0o644
    assert (tmp_path / "dtray-share" / "devices").read_text() == "strih-snv-intercom\nstrih-obs\n"
    assert r.stdout.splitlines()[0].endswith("strih-snv-intercom strih-obs, 2 marker(s) written"), r.stdout


def test_a_device_no_longer_declared_is_removed_and_a_foreign_marker_kept(tmp_path):
    assert _rc(_apply(tmp_path, SNV)) == 0
    d = tmp_path / "dstack-devices"
    (d / "someone-else.device").write_text("")
    r = _apply(tmp_path, "strih-obs=4440:8800:4455:47200-47231")
    assert _rc(r) == 0, r.stderr
    assert sorted(p.name for p in d.iterdir()) == ["someone-else.device", "strih-obs.device"]
    assert (tmp_path / "dtray-share" / "devices").read_text() == "strih-obs\n"
    r = _apply(tmp_path, "none")
    assert _rc(r) == 0, r.stderr
    assert sorted(p.name for p in d.iterdir()) == ["someone-else.device"]
    assert not (tmp_path / "dtray-share" / "devices").exists()


def test_a_declared_marker_someone_else_wrote_is_never_claimed(tmp_path):
    d = tmp_path / "dstack-devices"
    d.mkdir()
    (d / "strih-obs.device").write_text("")
    assert _rc(_apply(tmp_path, "strih-obs=4440:8800:4455:47200-47231")) == 0
    assert not (tmp_path / "dtray-share" / "devices").exists(), "not ours: never recorded"
    assert _rc(_apply(tmp_path, "none")) == 0
    assert (d / "strih-obs.device").exists(), "a foreign marker survives the box dropping the device"


def test_unreadable_facts_write_nothing(tmp_path):
    e = F.dtray_env(tmp_path)
    r = F.bash('unset DTRAY_DANTE_DEVICES; dtray_devices_apply && echo rc=0 || echo "rc=$?"', env=e,
               sources=(DTRAY_LIB,))
    assert _rc(r) == 1 and "box facts are unreadable" in r.stderr
    assert not (tmp_path / "dstack-devices").exists()


def test_the_markers_follow_the_strih_snv_fact_file():
    r = F.bash('unset DTRAY_DANTE_DEVICES; strih_box_load strih-snv >/dev/null || exit 9; dtray_expected_devices',
               sources=(F.SCRIPTS / "lib" / "strih-box-facts.sh", DTRAY_LIB))
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["strih-snv-intercom", "strih-obs"]
    for box in ("stream-snv", "strih-pp", "stream-pp"):
        r = F.bash('unset DTRAY_DANTE_DEVICES; strih_box_load %s >/dev/null || exit 9; dtray_expected_devices' % box,
                   sources=(F.SCRIPTS / "lib" / "strih-box-facts.sh", DTRAY_LIB))
        assert r.returncode == 0 and r.stdout == "", (box, r.stderr)


# --- the dtray --print grade ------------------------------------------------------------------------------

PRINT_OK = ("dstack: 2 devices, ok\n"
            "strih-snv-intercom: clock media ready; RX 4/4 connected; late +0; missed +0, overruns +0\n"
            "strih-obs: clock media ready; RX 2/2 connected; late +0\n")
PRINT_MISSING = ("dstack: 2 devices, strih-obs: expected, not running\n"
                 "strih-snv-intercom: clock media ready; RX 4/4 connected; late +0\n"
                 "strih-obs: expected, not running\n")


def _states(text, *names):
    r = F.bash('dtray_print_states %s' % " ".join(names), sources=(DTRAY_LIB,), stdin=text)
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def test_print_states_parse_dtrays_lines():
    assert _states(PRINT_OK, "strih-snv-intercom", "strih-obs") == ["present strih-snv-intercom", "present strih-obs"]
    assert _states(PRINT_MISSING, "strih-snv-intercom", "strih-obs") == [
        "present strih-snv-intercom", "not-running strih-obs"]
    assert _states("dstack: no devices on this PC\n", "strih-obs") == ["unlisted strih-obs"]
    # names compare case aside, as dtray does; a longer name sharing the prefix is not the device
    assert _states("STRIH-OBS: clock media ready\nstrih-obs-2: clock x\n", "strih-obs") == ["present strih-obs"]
    assert _states("strih-obs-2: clock x\n", "strih-obs") == ["unlisted strih-obs"]


def _row(tmp_path, devices, print_text=None, print_rc=0, hang=False, ic="v0.6.10", obs="v0.6.10", markers=True):
    e = F.dtray_env(tmp_path)
    e["DTRAY_DANTE_DEVICES"] = devices
    out = tmp_path / "print.out"
    out.write_text(print_text or "")
    b = F.dtray_bin(tmp_path)
    b.parent.mkdir(parents=True, exist_ok=True)
    b.write_text("#!/bin/bash\n[ \"$1\" = --print ] || exit 2\n%scat '%s'\nexit %d\n"
                 % ("sleep 30\n" if hang else "", out, print_rc))
    b.chmod(0o755)
    for name, val in (("ic.pin", "INTERCOM_RELEASE_DSTACK_VERSION=%s\n" % ic),
                      ("obs.pin", "OBS_RELEASE_DSTACK_VERSION=%s\n" % obs)):
        (tmp_path / name).write_text(val)
    e["DTRAY_INTERCOM_PIN_FILE"] = str(tmp_path / "ic.pin")
    e["DTRAY_OBS_PIN_FILE"] = str(tmp_path / "obs.pin")
    e["DTRAY_PRINT_TIMEOUT"] = "2"
    if markers:
        assert _rc(_dt('dtray_devices_apply && echo rc=0 || echo "rc=$?"', env=e)) == 0
    r = _dt("dtray_devices_row", env=e)
    assert r.returncode == 0 and r.stderr == "", r.stderr
    lines = r.stdout.splitlines()
    assert len(lines) == 1, lines
    return lines[0]


def test_row_ok_on_a_box_without_a_dante_device(tmp_path):
    row = _row(tmp_path, "none", print_rc=7)  # --print is never run there
    assert row.startswith("OK|(dtray-devices) no Dante device on this box"), row


def test_row_ok_when_dtray_shows_every_device(tmp_path):
    row = _row(tmp_path, SNV, PRINT_OK)
    assert row == "OK|(dtray-devices) markers in place and dtray --print shows every expected device: strih-snv-intercom strih-obs"


def test_row_note_while_a_pinned_dstack_is_below_the_first_dtray_sees(tmp_path):
    row = _row(tmp_path, SNV, PRINT_MISSING, ic="v0.6.19", obs="v0.6.10")
    assert row.startswith("NOTE|(dtray-devices) "), row
    assert "strih-obs" in row and "intercom v0.6.19 obs v0.6.10 first v0.6.13" in row


def test_row_fail_once_both_pins_reach_the_first(tmp_path):
    row = _row(tmp_path, SNV, PRINT_MISSING, ic="v0.6.13", obs="v0.6.19")
    assert row.startswith("FAIL|(dtray-devices) dtray --print misses expected device(s): strih-obs(not running)"), row


def test_row_fail_when_dtray_never_lists_a_marked_device(tmp_path):
    row = _row(tmp_path, SNV, "dstack: no devices on this PC\n")
    assert row.startswith("FAIL|(dtray-devices) ") and "strih-obs(not listed)" in row
    assert "strih-snv-intercom(not listed)" in row


def test_row_fail_on_a_failed_or_hung_print(tmp_path):
    assert _row(tmp_path, SNV, PRINT_OK, print_rc=1).startswith("FAIL|(dtray-devices) `dtray --print` failed (rc 1)")
    row = _row(tmp_path / "h", SNV, PRINT_OK, hang=True)
    assert row.startswith("FAIL|(dtray-devices) `dtray --print` failed (rc 124, no answer in 2 s)"), row


def test_row_fail_on_a_missing_or_stale_marker(tmp_path):
    row = _row(tmp_path, SNV, PRINT_OK, markers=False)
    assert row.startswith("FAIL|(dtray-devices) device markers") and "strih-snv-intercom strih-obs" in row
    # a marker of ours the facts no longer declare
    e = F.dtray_env(tmp_path / "s")
    e["DTRAY_DANTE_DEVICES"] = SNV
    assert _rc(_dt('dtray_devices_apply && echo rc=0 || echo "rc=$?"', env=e)) == 0
    e["DTRAY_DANTE_DEVICES"] = "none"
    r = _dt("dtray_devices_row", env=e)
    assert r.stdout.startswith("FAIL|(dtray-devices) device markers") and "stale of ours [ strih-snv-intercom strih-obs ]" in r.stdout


def test_the_threshold_is_read_from_the_shipped_pins():
    r = _dt('dtray_device_shows && echo " rc=0" || echo " rc=$?"')
    ic = [ln.split("=", 1)[1] for ln in (REPO / "scripts/intercom-release.pin").read_text().splitlines()
          if ln.startswith("INTERCOM_RELEASE_DSTACK_VERSION=")][0]
    obs = [ln.split("=", 1)[1] for ln in (REPO / "scripts/obs-release.pin").read_text().splitlines()
           if ln.startswith("OBS_RELEASE_DSTACK_VERSION=")][0]
    assert r.stdout.startswith("intercom %s obs %s first v0.6.13 rc=" % (ic, obs)), r.stdout


# --- the session app: unit, lists, install, grade ----------------------------------------------------------

def _unit():
    out = {}
    for ln in UNIT_FILE.read_text().splitlines():
        if "=" in ln and not ln.startswith("#"):
            out[ln.split("=", 1)[0]] = ln.split("=", 1)[1]
    return out


def test_the_unit_text():
    u = _unit()
    assert u["ExecStart"] == "/usr/local/bin/dtray" and u["ConditionFileIsExecutable"] == u["ExecStart"]
    assert u["Type"] == "simple" and u["Environment"] == "DISPLAY=:0"
    assert u["PartOf"] == "graphical-session.target" and u["After"] == "graphical-session.target"
    assert u["WantedBy"] == "graphical-session.target"
    # dtray exits 1 without a desktop: a bounded restart, never a loop
    assert u["Restart"] == "on-failure" and int(u["StartLimitBurst"]) <= 10 and int(u["StartLimitIntervalSec"]) > 0
    r = _dt("dtray_bin", env={"DTRAY_BIN": ""})
    assert r.stdout == u["ExecStart"]


def test_dtray_is_the_last_session_app_on_both_roles():
    r = F.bash('printf "%s\\n" "${STRIH_SESSION_APP_UNITS[@]}"; strih_session_app_item dtray.service; echo;'
               'printf "[%s]" "$(strih_session_app_script dtray.service)"; echo; strih_session_app_program dtray.service',
               env={"DTRAY_BIN": ""})
    lines = r.stdout.splitlines()
    assert lines[:7] == list(F.UNITS) and lines[6] == UNIT
    assert lines[7:] == ["dtray", "[]", "/usr/local/bin/dtray"]
    role = (F.SCRIPTS / "lib" / "obs-box-role.sh").read_text()
    line = [ln for ln in role.splitlines() if ln.startswith("OBS_BOX_STREAM_SESSION_APP_UNITS=(")][0]
    assert line.endswith(" dtray.service)"), line
    assert "systemctl --user start dtray.service || true" in F.bash("strih_session_apps_autostart_lines").stdout.splitlines()


def test_install_through_the_session_app_loop(tmp_path):
    home = tmp_path / "home"
    b, log = F.fake_bin(tmp_path)
    env = {"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "usrlocalbin"), "STRIH_SESSION_APPS_EUID": "1000",
           "STRIH_SESSION_APPS_PYTHON": str(F.fake_python_ok(tmp_path))}
    env.update(_env(tmp_path))
    env["DTRAY_DANTE_DEVICES"] = SNV
    r = F.bash('strih_session_apps_install "%s" "%s" newlevel' % (REPO, home), env=env, path_prepend=str(b))
    assert r.returncode == 0, r.stdout + r.stderr
    assert F.dtray_bin(tmp_path).read_bytes() == F.DTRAY_GOOD
    assert (home / ".config/systemd/user" / UNIT).read_bytes() == UNIT_FILE.read_bytes()
    assert sorted(p.name for p in (tmp_path / "dstack-devices").iterdir()) == ["strih-obs.device",
                                                                              "strih-snv-intercom.device"]
    calls = log.read_text()
    assert any(ln.startswith("systemctl --user enable ") and UNIT in ln.split() for ln in calls.splitlines())
    restart = [ln for ln in calls.splitlines() if ln.startswith("systemctl --user try-restart ")]
    assert restart and UNIT in restart[0].split()
    for ln in calls.splitlines():
        if ln.startswith("systemctl"):
            assert "start" not in ln.split() and "--now" not in ln.split(), ln
    # a second run changes nothing: no restart
    shutil.rmtree(tmp_path / "bin")
    b, log = F.fake_bin(tmp_path)
    r = F.bash('strih_session_apps_install "%s" "%s" newlevel' % (REPO, home), env=env, path_prepend=str(b))
    assert r.returncode == 0, r.stderr
    assert "try-restart" not in log.read_text()


def test_install_a_failed_dtray_fetch_warns_and_provisions_the_rest(tmp_path):
    home = tmp_path / "home"
    b, _log = F.fake_bin(tmp_path)
    env = {"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "usrlocalbin"), "STRIH_SESSION_APPS_EUID": "1000",
           "STRIH_SESSION_APPS_PYTHON": str(F.fake_python_ok(tmp_path))}
    env.update(F.dtray_env(tmp_path))
    env["DTRAY_RELEASE_BASE"] = "file://%s/none" % tmp_path
    r = F.bash('strih_session_apps_install "%s" "%s" newlevel' % (REPO, home), env=env, path_prepend=str(b))
    assert r.returncode == 0, r.stdout + r.stderr
    assert "WARN: the dtray step failed" in r.stdout
    assert [ln for ln in r.stdout.splitlines() if "WARN" in ln and "dtray" in ln] == [
        "  WARN: the dtray step failed (above) -- verify-strih (dtray) names what is missing; re-run setup-strih.sh step 16d"]
    for unit in F.UNIT_FILES:
        assert (home / ".config/systemd/user" / unit).is_file(), unit


def _grade(tmp_path, env_extra=None, **fakes):
    home, bindir = F.installed_home(tmp_path)
    F.all_states(tmp_path / "run")
    b, _ = F.fake_bin(tmp_path, wmctrl=F.WIN, **fakes)
    env = F.grade_env(tmp_path, bindir, tmp_path / "run")
    env.update(env_extra or {})
    r = F.bash('strih_session_apps_grade_rows "%s" "%s" newlevel' % (REPO, home), env=env, path_prepend=str(b))
    assert r.returncode == 0 and r.stderr == "", r.stderr
    return r.stdout.splitlines()


def test_grade_rows_ok(tmp_path):
    rows = _grade(tmp_path)
    b = F.dtray_bin(tmp_path)
    assert F.row(rows, "dtray") == (
        "OK|(dtray) %s: unit matches the checkout, %s installed, enabled, started by the kiosk autostart, "
        "active, its process runs %s and started after the unit was installed" % (UNIT, b, b))
    assert F.row(rows, "dtray-devices").startswith("OK|(dtray-devices) no Dante device")
    r = F.bash("strih_session_apps_row_count")
    assert len(rows) == int(r.stdout) == len(F.UNITS) + 6


def test_grade_row_fails_a_dtray_behind_the_pin(tmp_path):
    newer = F.dtray_pin(tmp_path / "p", tag="v0.6.20")
    rows = _grade(tmp_path, env_extra={"DTRAY_PIN_FILE": str(newer)})
    row = F.row(rows, "dtray")
    assert row.startswith("FAIL|(dtray) %s: script-differs" % UNIT), row
    assert "program=stale:installed-v0.6.19-pin-v0.6.20" in row and "step 16d" in row


def test_grade_row_fails_a_missing_dtray(tmp_path):
    rows = _grade(tmp_path, env_extra={"DTRAY_BIN": str(tmp_path / "nope" / "dtray")})
    row = F.row(rows, "dtray")
    assert row.startswith("FAIL|(dtray) %s: script-absent" % UNIT) and "dstack" in row, row


def test_grade_row_fails_an_inactive_dtray(tmp_path):
    rows = _grade(tmp_path, active="failed")
    assert F.row(rows, "dtray").startswith("FAIL|(dtray) %s: not-active:failed" % UNIT)
