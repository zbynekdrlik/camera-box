"""Issue 1542 (intercom#34) -- the intercom project's tray icon as a session app on the STRIH role.

What this pins:
  * scripts/intercom-release.pin = intercom v1.9.26 (the release with the 7th bundle member
    `intercom-tray`), and STRIH_INTERCOM_FILES lists that member (a bundle without it is refused);
  * scripts/lib/intercom-tray.sh `intercom_tray_install`: the tray only from the pinned bundle that
    strih_intercom_release_ensure proved, kept when already that member, renamed into place (a running
    tray keeps its inode), a release record for a network-free verify; `intercom_tray_state`
    (absent / stale:<why> / ok), the binary checked against the pinned bundle's member when present;
  * scripts/lib/strih-session-apps.sh: `intercom-tray.service` LAST on the strih role, never on the
    stream role; its unit; the install through the loop and its grader row.

Tier-0: bash + pytest only. The bundle is a local tarball (STRIH_INTERCOM_TARBALL_SRC) proved against
a fixture pin; no network, no root. Everything runs under the callers' set -euo pipefail (F.bash).
"""
import hashlib
import os
import stat
import subprocess
import sys
import tarfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import strih_session_apps_fakes_1399 as F  # noqa: E402

REPO = F.REPO
TRAY_LIB = F.SCRIPTS / "lib" / "intercom-tray.sh"
RELEASE_LIB = F.SCRIPTS / "lib" / "strih-intercom-release.sh"
UNIT_FILE = REPO / "systemd" / "intercom-tray.service"
UNIT = "intercom-tray.service"
PINNED = REPO / "tests" / "fixtures" / "intercom_pinned"
MEMBERS = ("intercom.strih-snv.toml", "intercom.strih-pp.toml", "intercom.stream-snv.toml",
           "intercom.stream-pp.toml", "intercom-hub.service")
TRAY = b"#!/bin/sh\n# intercom-tray (fake bundle member)\n"


def _bundle(tmp_path, version="9.9.42", tray=TRAY, with_tray=True):
    """A bundle tarball + a pin naming it; returns the env that makes the libs use them."""
    d = tmp_path / "bundle-src"
    d.mkdir(exist_ok=True)
    hub = d / "intercom-hub"
    hub.write_text("#!/bin/sh\n[ \"${1:-}\" = --version ] && echo 'intercom-hub %s' && exit 0\nexit 3\n" % version)
    hub.chmod(0o755)
    (d / "intercom-tray").write_bytes(tray)
    (d / "intercom-tray").chmod(0o755)
    tb = d / "bundle.tar.gz"
    with tarfile.open(tb, "w:gz") as t:
        t.add(hub, arcname="intercom-hub")
        if with_tray:
            t.add(d / "intercom-tray", arcname="intercom-tray")
        for f in MEMBERS:
            t.add(PINNED / f, arcname=f)
    pin = d / "pin"
    pin.write_text("INTERCOM_RELEASE_TAG=v%s\nINTERCOM_RELEASE_SHA256=%s\n"
                   % (version, hashlib.sha256(tb.read_bytes()).hexdigest()))
    return {"STRIH_INTERCOM_PIN_FILE": str(pin), "STRIH_INTERCOM_TARBALL_SRC": str(tb),
            "INTERCOM_TRAY_BIN": str(F.itray_bin(tmp_path)), "INTERCOM_TRAY_RELEASE_FILE": str(F.itray_record(tmp_path))}


def _tree(tmp_path):
    root = tmp_path / "tree"
    root.mkdir(exist_ok=True)
    return root


def _tray(body, env=None):
    return F.bash(body, env=env, sources=(TRAY_LIB,))


def _rc(r):
    assert r.stdout.splitlines(), r.stderr
    return int(r.stdout.splitlines()[-1].rsplit("=", 1)[1])


def _install(tmp_path, env):
    return _tray('intercom_tray_install "%s" && echo rc=0 || echo "rc=$?"' % _tree(tmp_path), env=env)


def _state(tmp_path, env):
    r = _tray('intercom_tray_state "%s" && echo " rc=0" || echo " rc=$?"' % _tree(tmp_path), env=env)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


# --- the pin + the bundle -------------------------------------------------------------------------------

def test_the_shipped_pin_carries_the_tray_release_or_later():
    # issue 1542: the tray is in the hub bundle from intercom v1.9.26 on; a later pin keeps it
    pin = dict(ln.split("=", 1) for ln in (REPO / "scripts/intercom-release.pin").read_text().splitlines() if ln)
    assert set(pin) == {"INTERCOM_RELEASE_TAG", "INTERCOM_RELEASE_SHA256", "INTERCOM_RELEASE_DSTACK_VERSION"}
    tag = pin["INTERCOM_RELEASE_TAG"]
    assert tuple(int(x) for x in tag.lstrip("v").split(".")) >= (1, 9, 26), tag
    assert len(pin["INTERCOM_RELEASE_SHA256"]) == 64
    assert ('intercom-vban = { git = "https://github.com/zbynekdrlik/intercom", tag = "%s" }' % tag) in \
        (REPO / "Cargo.toml").read_text()
    # the routing files of that release (the sha256 the release ships, the ticket's)
    for name, sha in (("intercom.strih-snv.toml", "35d1730e75990acb340145703246f4672287061802cdf83bfb5b6345979a0670"),
                      ("intercom.strih-pp.toml", "15dcf4afd22eaa1691fc43db62eecd9fc9224fac0eb80a17a142218f92cfc223")):
        assert hashlib.sha256((PINNED / name).read_bytes()).hexdigest() == sha, name


def test_the_tray_is_an_optional_bundle_member():
    r = F.bash('printf "%s\\n" "${STRIH_INTERCOM_FILES[@]}"; echo --; printf "%s\\n" "${STRIH_INTERCOM_OPTIONAL_FILES[@]}"',
               sources=(RELEASE_LIB,))
    assert r.returncode == 0, r.stderr
    req, opt = r.stdout.split("--\n")
    assert "intercom-tray" not in req.split() and len(req.split()) == 6
    assert opt.split() == ["intercom-tray"]


def test_a_bundle_without_the_tray_still_installs_the_hub(tmp_path):
    """A hub pin rolled back below v1.9.26 (no tray member) must still give step 13 its bundle."""
    env = _bundle(tmp_path, with_tray=False)
    root = _tree(tmp_path)
    stale = root / "intercom-release" / "v9.9.42" / "intercom-tray"
    stale.parent.mkdir(parents=True)
    stale.write_bytes(b"left by another extraction")
    r = F.bash('strih_intercom_release_ensure "%s"' % root, env=env, sources=(RELEASE_LIB,))
    assert r.returncode == 0, r.stderr
    d = Path(r.stdout.strip())
    assert (d / "intercom-hub").is_file() and (d / "intercom.strih-snv.toml").is_file()
    assert not stale.exists(), "a member this release does not carry stays from nothing"


# --- intercom_tray_install -----------------------------------------------------------------------------

def test_the_pinned_bundle_installs_the_tray_and_its_record(tmp_path):
    env = _bundle(tmp_path)
    r = _install(tmp_path, env)
    assert _rc(r) == 0, r.stderr
    b = F.itray_bin(tmp_path)
    assert b.read_bytes() == TRAY and stat.S_IMODE(b.stat().st_mode) == 0o755
    assert "installed intercom-tray v9.9.42 -> %s" % b in r.stdout
    assert F.itray_record(tmp_path).read_text() == "v9.9.42 %s\n" % hashlib.sha256(TRAY).hexdigest()
    assert sorted(p.name for p in b.parent.iterdir()) == ["intercom-tray"], "no .new sibling left"
    # the bundle landed in the tree's own release dir, the tray among its members
    assert (_tree(tmp_path) / "intercom-release" / "v9.9.42" / "intercom-tray").read_bytes() == TRAY


def test_the_installed_member_is_kept_with_its_mtime(tmp_path):
    env = _bundle(tmp_path)
    assert _rc(_install(tmp_path, env)) == 0
    b = F.itray_bin(tmp_path)
    past = 1_700_000_000
    os.utime(b, (past, past))
    r = _install(tmp_path, env)
    assert _rc(r) == 0, r.stderr
    assert "intercom-tray v9.9.42 present" in r.stdout and "kept" in r.stdout
    assert b.stat().st_mtime == past


def test_another_tray_is_replaced_by_rename_so_a_running_one_keeps_its_inode(tmp_path):
    b = F.fake_intercom_tray(tmp_path, content=b"#!/bin/sh\n# old tray\n")
    held = tmp_path / "held-by-the-running-tray"
    os.link(b, held)  # what a running process keeps open: the old inode
    old_ino = b.stat().st_ino
    r = _install(tmp_path, _bundle(tmp_path))
    assert _rc(r) == 0, r.stderr
    assert b.read_bytes() == TRAY and b.stat().st_ino != old_ino
    assert held.read_bytes() == b"#!/bin/sh\n# old tray\n", "the old inode was rewritten in place"
    assert "replaced intercom-tray with v9.9.42" in r.stdout


def test_a_bundle_without_the_tray_installs_nothing(tmp_path):
    r = _install(tmp_path, _bundle(tmp_path, with_tray=False))
    assert _rc(r) == 1
    assert "has no intercom-tray" in r.stderr
    assert not F.itray_bin(tmp_path).exists()


def test_a_tarball_that_is_not_the_pin_installs_nothing_and_leaves_an_installed_tray(tmp_path):
    b = F.fake_intercom_tray(tmp_path, content=b"#!/bin/sh\n# old tray\n")
    before = (b.read_bytes(), b.stat().st_mtime_ns)
    env = _bundle(tmp_path)
    Path(env["STRIH_INTERCOM_PIN_FILE"]).write_text("INTERCOM_RELEASE_TAG=v9.9.42\nINTERCOM_RELEASE_SHA256=%s\n" % ("a" * 64))
    r = _install(tmp_path, env)
    assert _rc(r) == 1 and "the pin says" in r.stderr
    assert (b.read_bytes(), b.stat().st_mtime_ns) == before


# --- intercom_tray_state -------------------------------------------------------------------------------

def test_state_reads_ok_after_the_install_and_absent_without_a_binary(tmp_path):
    env = _bundle(tmp_path)
    assert _state(tmp_path, env) == "absent rc=1"
    assert _rc(_install(tmp_path, env)) == 0
    assert _state(tmp_path, env) == "ok rc=0"


def test_state_names_each_stale_case(tmp_path):
    env = _bundle(tmp_path)
    assert _rc(_install(tmp_path, env)) == 0
    rec = F.itray_record(tmp_path)
    good = rec.read_text()
    rec.unlink()
    assert _state(tmp_path, env) == "stale:no-release-record rc=1"
    rec.write_text(good.replace("v9.9.42", "v9.9.41"))
    assert _state(tmp_path, env) == "stale:installed-v9.9.41-pin-v9.9.42 rc=1"
    rec.write_text(good)
    F.itray_bin(tmp_path).write_bytes(b"#!/bin/sh\n# tampered\n")
    assert _state(tmp_path, env) == "stale:sha256-is-not-the-recorded-v9.9.42 rc=1"
    # binary + record agree with each other, but not with the pinned bundle's member
    other = b"#!/bin/sh\n# another build\n"
    F.itray_bin(tmp_path).write_bytes(other)
    rec.write_text("v9.9.42 %s\n" % hashlib.sha256(other).hexdigest())
    assert _state(tmp_path, env) == "stale:not-the-v9.9.42-bundle-member rc=1"


def test_state_without_an_extracted_bundle_grades_by_the_record(tmp_path):
    F.fake_intercom_tray(tmp_path)
    assert _state(tmp_path, F.intercom_tray_env(tmp_path)) == "ok rc=0"


# --- the session app ------------------------------------------------------------------------------------

def test_the_unit():
    u = UNIT_FILE.read_text()
    for line in ("PartOf=graphical-session.target", "After=graphical-session.target",
                 "ConditionFileIsExecutable=/usr/local/bin/intercom-tray", "StartLimitIntervalSec=300",
                 "StartLimitBurst=5", "Type=simple", "Environment=DISPLAY=:0",
                 "ExecStart=/usr/local/bin/intercom-tray", "Restart=on-failure", "RestartSec=10",
                 "StandardError=journal", "SyslogIdentifier=intercom-tray", "WantedBy=graphical-session.target"):
        assert line in u.splitlines(), line
    assert "After=strih-obs.service" not in u  # the login ordering cycle (issue 1465)


def test_it_is_the_last_session_app_on_the_strih_role_only():
    r = F.bash('printf "%s\\n" "${STRIH_SESSION_APP_UNITS[@]}"; strih_session_app_item intercom-tray.service; echo;'
               'printf "[%s]" "$(strih_session_app_script intercom-tray.service)"; echo;'
               'strih_session_app_program intercom-tray.service', env={"INTERCOM_TRAY_BIN": ""})
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert lines[:8] == list(F.UNITS) and lines[7] == UNIT
    assert lines[8:] == ["intercom-tray", "[]", "/usr/local/bin/intercom-tray"]
    assert "systemctl --user start intercom-tray.service || true" in \
        F.bash("strih_session_apps_autostart_lines").stdout.splitlines()
    role = (F.SCRIPTS / "lib" / "obs-box-role.sh").read_text()
    stream = [ln for ln in role.splitlines() if ln.startswith("OBS_BOX_STREAM_SESSION_APP_UNITS=(")][0]
    assert "intercom-tray" not in stream


def _start_units(box):
    fleet = F.SCRIPTS / "deploy-genlock-fleet.sh"
    script = 'set -euo pipefail\n. "%s"\nstrih_lx_remote_start_cmd /tmp/stage "$1"\n' % fleet
    r = subprocess.run(["bash", "-c", script, "harness", box], capture_output=True, text=True, cwd=REPO,
                       env={"PATH": os.environ["PATH"], "HOME": "/home/newlevel", "LC_ALL": "C.UTF-8"}, timeout=60)
    assert r.returncode == 0, r.stderr
    tail = r.stdout.split("systemctl --user start strih-obs.service", 1)[1]
    return tail.split("systemctl --user start ", 1)[1].split(" ||", 1)[0].split()


def test_the_deploy_starts_it_on_the_strih_boxes_and_never_on_the_stream_boxes():
    for box in ("strih-snv", "strih-pp"):
        assert _start_units(box)[-1] == UNIT, box
    for box in ("stream-snv", "stream-pp"):
        assert UNIT not in _start_units(box), box


def _install_env(tmp_path):
    env = {"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "usrlocalbin"), "STRIH_SESSION_APPS_EUID": "1000",
           "STRIH_SESSION_APPS_PYTHON": str(F.fake_python_ok(tmp_path))}
    env.update(_bundle(tmp_path))
    return env


def test_install_through_the_session_app_loop(tmp_path):
    home = tmp_path / "home"
    b, log = F.fake_bin(tmp_path)
    env = _install_env(tmp_path)
    # the loop installs from the tree it is handed (setup-strih: the checkout); a fixture tree holding the
    # unit + program files keeps the test off the real checkout's intercom-release dir
    tree = _tree(tmp_path)
    os.symlink(REPO / "systemd", tree / "systemd")
    os.symlink(REPO / "scripts", tree / "scripts")
    r = F.bash('strih_session_apps_install "%s" "%s" newlevel' % (tree, home), env=env, path_prepend=str(b))
    assert r.returncode == 0, r.stdout + r.stderr
    assert F.itray_bin(tmp_path).read_bytes() == TRAY
    assert (home / ".config/systemd/user" / UNIT).read_bytes() == UNIT_FILE.read_bytes()
    assert "installed %s -> %s/.config/systemd/user/%s (runs %s): binary written, unit written" % (
        UNIT, home, UNIT, F.itray_bin(tmp_path)) in r.stdout
    calls = log.read_text().splitlines()
    assert any(ln.startswith("systemctl --user enable ") and UNIT in ln.split() for ln in calls)
    restart = [ln for ln in calls if ln.startswith("systemctl --user try-restart ")]
    assert restart and UNIT in restart[0].split()
    for ln in calls:
        if ln.startswith("systemctl"):
            assert "start" not in ln.split() and "--now" not in ln.split(), ln


def test_a_failed_tray_install_warns_and_provisions_the_rest(tmp_path):
    home = tmp_path / "home"
    b, _log = F.fake_bin(tmp_path)
    env = {"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "usrlocalbin"), "STRIH_SESSION_APPS_EUID": "1000",
           "STRIH_SESSION_APPS_PYTHON": str(F.fake_python_ok(tmp_path))}
    env.update(F.intercom_tray_env(tmp_path))  # a pin whose tarball nobody serves (the hermetic URL)
    tree = _tree(tmp_path)  # the failed fetch's release dir lands here, never in the checkout
    os.symlink(REPO / "systemd", tree / "systemd")
    os.symlink(REPO / "scripts", tree / "scripts")
    r = F.bash('strih_session_apps_install "%s" "%s" newlevel' % (tree, home), env=env, path_prepend=str(b))
    assert r.returncode == 0, r.stdout + r.stderr
    assert [ln for ln in r.stdout.splitlines() if "WARN" in ln and "intercom-tray" in ln] == [
        "  WARN: the intercom-tray step failed (above) -- verify-strih (intercom-tray) names what is missing; "
        "re-run setup-strih.sh step 16d"]
    for unit in F.UNIT_FILES:
        assert (home / ".config/systemd/user" / unit).is_file(), unit


# --- the grader row --------------------------------------------------------------------------------------

def _grade(tmp_path, env_extra=None, **fakes):
    home, bindir = F.installed_home(tmp_path)
    F.all_states(tmp_path / "run")
    b, _ = F.fake_bin(tmp_path, wmctrl=F.WIN, **fakes)
    env = F.grade_env(tmp_path, bindir, tmp_path / "run")
    env.update(env_extra or {})
    r = F.bash('strih_session_apps_grade_rows "%s" "%s" newlevel' % (REPO, home), env=env, path_prepend=str(b))
    assert r.returncode == 0 and r.stderr == "", r.stderr
    return r.stdout.splitlines()


def test_grade_row_ok(tmp_path):
    rows = _grade(tmp_path)
    b = F.itray_bin(tmp_path)
    assert F.row(rows, "intercom-tray") == (
        "OK|(intercom-tray) %s: unit matches the checkout, %s installed, enabled, started by the kiosk autostart, "
        "active, its process runs %s and started after the unit was installed" % (UNIT, b, b))
    assert len(rows) == int(F.bash("strih_session_apps_row_count").stdout) == len(F.UNITS) + 6


def test_grade_row_fails_a_tray_behind_the_pin(tmp_path):
    (tmp_path / "p").mkdir()
    newer = F.itray_pin(tmp_path / "p", tag="v9.9.1543")
    rows = _grade(tmp_path, env_extra={"STRIH_INTERCOM_PIN_FILE": str(newer)})
    row = F.row(rows, "intercom-tray")
    assert row.startswith("FAIL|(intercom-tray) %s: script-differs" % UNIT), row
    assert "program=stale:installed-v9.9.1542-pin-v9.9.1543" in row and "intercom hub bundle" in row


def test_grade_row_fails_a_missing_tray(tmp_path):
    rows = _grade(tmp_path, env_extra={"INTERCOM_TRAY_BIN": str(tmp_path / "nope" / "intercom-tray")})
    row = F.row(rows, "intercom-tray")
    assert row.startswith("FAIL|(intercom-tray) %s: script-absent" % UNIT) and "step 16d" in row, row


def test_grade_row_fails_a_process_that_runs_another_binary(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    exe = tmp_path / "proc" / str(F.PIDS[UNIT]) / "exe"
    exe.unlink()
    os.symlink("/usr/bin/true", exe)
    F.all_states(tmp_path / "run")
    b, _ = F.fake_bin(tmp_path, wmctrl=F.WIN)
    r = F.bash('strih_session_apps_grade_rows "%s" "%s" newlevel' % (REPO, home),
               env=F.grade_env(tmp_path, bindir, tmp_path / "run"), path_prepend=str(b))
    assert r.returncode == 0, r.stderr
    assert F.row(r.stdout.splitlines(), "intercom-tray").startswith(
        "FAIL|(intercom-tray) %s: process:wrong-program" % UNIT)


def test_grade_row_fails_an_inactive_tray(tmp_path):
    rows = _grade(tmp_path, active="failed")
    assert F.row(rows, "intercom-tray").startswith("FAIL|(intercom-tray) %s: not-active:failed" % UNIT)
