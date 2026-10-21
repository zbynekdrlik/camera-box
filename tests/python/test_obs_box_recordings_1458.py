"""issue 1458 (owner 21.10.2026): a file manager in the Linux OBS boxes' panel to delete old OBS recordings.

"este jedna vec ktoru chcem aby si spravil aby taktiez v liste na linuxoch boli file explorer aby vedeli
vymazat nahravky ktore robi obs ked uz je ich tam vela". Design comment 6320960771:
  * scripts/obs_box_recordings.py (std-only): `print` / `open` the ACTIVE OBS profile's record folder,
    resolved the way OBS does at click time (stream-pp's profile has no RecFilePath at all), and
    `seed-libfm` / `check-libfm` for pcmanfm's `use_trash=0` + `confirm_del=1` (a delete frees the disk
    and still asks first);
  * the panel lib (issue 1420): the `Nahrávky` launcher where pcmanfm exists, pcmanfm + the helper + the
    libfm seed in step 16h, the `(panel-files)` row in verify item 44.
"""
import os
import subprocess
import sys
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_obs_box_panel_1420 import REPO, Box, _bash, _exec, _healthy, _rows, box  # noqa: E402,F401

HELPER = REPO / "scripts" / "obs_box_recordings.py"


def _home(tmp_path, ini_basic=None, user_ini="[Basic]\nProfileDir=stream-pp\n", videos=False):
    home = tmp_path / "h"
    cfg = home / ".config" / "obs-studio"
    (cfg / "basic" / "profiles" / "stream-pp").mkdir(parents=True)
    if user_ini is not None:
        (cfg / "user.ini").write_text(user_ini)
    if ini_basic is not None:
        (cfg / "basic" / "profiles" / "stream-pp" / "basic.ini").write_text(ini_basic)
    if videos:
        (home / "Videos").mkdir()
    return home


def _run(home, *args, env=None):
    e = dict(os.environ, HOME=str(home))
    e.update(env or {})
    return subprocess.run([sys.executable, str(HELPER), *args], capture_output=True, text=True, env=e, timeout=30)


# --- the record folder, resolved like OBS --------------------------------------------------------------

def test_an_advanced_profile_opens_its_rec_file_path(tmp_path):
    rec = tmp_path / "rec"
    rec.mkdir()
    home = _home(tmp_path, "[Output]\nMode=Advanced\n\n[AdvOut]\nRecType=Standard\nRecFilePath=%s\n" % rec)
    r = _run(home, "print")
    assert r.returncode == 0 and r.stdout.strip() == str(rec), r.stdout + r.stderr


def test_an_ffmpeg_recording_opens_its_ff_file_path(tmp_path):
    rec, ff = tmp_path / "rec", tmp_path / "ff"
    rec.mkdir()
    ff.mkdir()
    home = _home(tmp_path, "[Output]\nMode=Advanced\n\n[AdvOut]\nRecType=FFmpeg\nRecFilePath=%s\nFFFilePath=%s\n"
                 % (rec, ff))
    assert _run(home, "print").stdout.strip() == str(ff)


def test_a_simple_profile_opens_its_file_path(tmp_path):
    rec = tmp_path / "simple"
    rec.mkdir()
    home = _home(tmp_path, "[Output]\nMode=Simple\n\n[SimpleOutput]\nFilePath=%s\n" % rec)
    assert _run(home, "print").stdout.strip() == str(rec)


def test_no_path_in_the_profile_is_obs_default_videos(tmp_path):
    """stream-pp 21.10.2026: Mode=Advanced and no RecFilePath -- OBS records into ~/Videos."""
    home = _home(tmp_path, "[Output]\nMode=Advanced\n", videos=True)
    assert _run(home, "print").stdout.strip() == str(home / "Videos")


def test_no_videos_folder_falls_back_to_home(tmp_path):
    home = _home(tmp_path, "[Output]\nMode=Advanced\n")
    assert _run(home, "print").stdout.strip() == str(home)


def test_a_path_that_does_not_exist_falls_back_and_says_so(tmp_path):
    home = _home(tmp_path, "[Output]\nMode=Simple\n\n[SimpleOutput]\nFilePath=/no/such/folder\n", videos=True)
    r = _run(home, "print")
    assert r.stdout.strip() == str(home / "Videos")
    assert "/no/such/folder" in r.stderr


def test_no_obs_config_at_all_opens_the_default(tmp_path):
    home = _home(tmp_path, None, user_ini=None, videos=True)
    assert _run(home, "print").stdout.strip() == str(home / "Videos")


def test_a_bom_and_crlf_user_ini_still_names_the_profile(tmp_path):
    rec = tmp_path / "rec"
    rec.mkdir()
    home = _home(tmp_path, "[Output]\r\nMode=Simple\r\n\r\n[SimpleOutput]\r\nFilePath=%s\r\n" % rec,
                 user_ini="﻿[Basic]\r\nProfileDir=stream-pp\r\n")
    assert _run(home, "print").stdout.strip() == str(rec)


def test_open_starts_the_file_manager_in_the_record_folder(tmp_path):
    rec = tmp_path / "rec"
    rec.mkdir()
    home = _home(tmp_path, "[Output]\nMode=Simple\n\n[SimpleOutput]\nFilePath=%s\n" % rec)
    log = tmp_path / "fm.log"
    fm = _exec(tmp_path / "fakefm", '#!/bin/bash\nprintf "%%s\\n" "$@" > %s\n' % log)
    r = _run(home, "open", env={"OBS_BOX_RECORDINGS_FM": str(fm)})
    assert r.returncode == 0, r.stderr
    assert log.read_text().splitlines() == [str(rec)]


def test_a_bad_command_is_usage_rc_2(tmp_path):
    assert _run(_home(tmp_path), "frobnicate").returncode == 2


# --- pcmanfm's libfm.conf: a delete frees the disk, after a confirmation -------------------------------

def test_seed_libfm_writes_a_fresh_conf(tmp_path):
    conf = tmp_path / "libfm" / "libfm.conf"
    r = _run(tmp_path, "seed-libfm", str(conf))
    assert r.returncode == 0 and "written" in r.stdout, r.stdout + r.stderr
    text = conf.read_text()
    assert "use_trash=0" in text and "confirm_del=1" in text and "[config]" in text
    assert _run(tmp_path, "check-libfm", str(conf)).returncode == 0


def test_seed_libfm_keeps_the_other_keys_and_is_idempotent(tmp_path):
    conf = tmp_path / "libfm.conf"
    conf.write_text("[config]\nsingle_click=1\nuse_trash=1\n\n[ui]\nbig_icon_size=48\n")
    assert "written" in _run(tmp_path, "seed-libfm", str(conf)).stdout
    text = conf.read_text()
    assert "single_click=1" in text and "big_icon_size=48" in text and "use_trash=0" in text
    assert "use_trash=1" not in text
    again = _run(tmp_path, "seed-libfm", str(conf))
    assert again.returncode == 0 and "unchanged" in again.stdout, again.stdout


def test_check_libfm_fails_a_trash_conf_and_a_missing_one(tmp_path):
    conf = tmp_path / "libfm.conf"
    conf.write_text("[config]\nuse_trash=1\nconfirm_del=1\n")
    r = _run(tmp_path, "check-libfm", str(conf))
    assert r.returncode == 1 and "use_trash" in r.stdout
    assert _run(tmp_path, "check-libfm", str(tmp_path / "none.conf")).returncode == 1


# --- the panel: launcher, install, row -----------------------------------------------------------------

def _files_env(b, present=True):
    fm = b.tmp / ("pcmanfm" if present else "no-pcmanfm-here")
    if present:
        _exec(fm, "#!/bin/sh\n")
    return {**b.env(), "OBS_BOX_PANEL_FILES_BIN": str(fm)}


def test_the_recordings_launcher_is_listed_where_pcmanfm_exists(box):
    box.with_obs()
    r = _bash('obs_box_panel_launchers "$1"', str(box.home), env=_files_env(box))
    rows = [ln.split("|") for ln in r.stdout.splitlines()]
    assert [x[0] for x in rows][:2] == ["obs", "files"], r.stdout + r.stderr
    files = dict((x[0], x) for x in rows)["files"]
    assert files[1] == "Nahrávky"
    assert files[2] == "/usr/bin/python3 %s open" % (box.tmp / "lib" / "camera-box" / "obs_box_recordings.py")
    r = _bash('obs_box_panel_launchers "$1"', str(box.home), env=_files_env(box, present=False))
    assert "files|" not in r.stdout


def test_install_adds_pcmanfm_the_helper_and_the_no_trash_conf(box):
    r = _bash('obs_box_panel_install "$1" "$2" "$(id -un)"', str(REPO), str(box.home), env=_files_env(box))
    assert r.returncode == 0, r.stdout + r.stderr
    assert "apt-get install -y tint2" in box.calls()
    helper = box.tmp / "lib" / "camera-box" / "obs_box_recordings.py"
    assert helper.read_bytes() == HELPER.read_bytes()
    conf = box.home / ".config" / "libfm" / "libfm.conf"
    assert "use_trash=0" in conf.read_text()


def test_install_asks_apt_for_pcmanfm_when_it_is_missing(box):
    # per-package stubs (the shared Box stub reads one "installed" flag for every package): tint2 and
    # discord are already there, pcmanfm is not, so exactly pcmanfm is installed
    pk = box.tmp / "pkgbin"
    pk.mkdir()
    st = box.tmp / "pkgs"
    st.mkdir()
    for have in ("tint2", "discord"):   # discord: the stream role's pinned .deb, already there
        (st / have).write_text("")
    _exec(pk / "dpkg-query", '#!/bin/bash\n[ -e %s/"${@: -1}" ] && printf "install ok installed"\nexit 0\n' % st)
    _exec(pk / "apt-get", '#!/bin/bash\necho "apt-get $*" >> %s\ntouch %s/"${@: -1}"\n' % (box.log, st))
    env = _files_env(box)
    env["PATH"] = "%s:%s" % (pk, env["PATH"])
    r = _bash('obs_box_panel_install "$1" "$2" "$(id -un)"', str(REPO), str(box.home), env=env)
    assert r.returncode == 0, r.stderr
    apt = [ln for ln in box.calls().splitlines() if ln.startswith("apt-get")]
    assert "apt-get install -y pcmanfm" in apt and "apt-get install -y tint2" not in apt, apt


def test_the_files_row_passes_healthy_and_fails_a_trash_conf(box):
    _healthy(box)
    rows = dict((d.split(")", 1)[0][1:], (st, d)) for st, d in _rows(box))
    assert rows["panel-files"][0] == "OK", rows["panel-files"]
    (box.home / ".config" / "libfm" / "libfm.conf").write_text("[config]\nuse_trash=1\n")
    rows = dict((d.split(")", 1)[0][1:], (st, d)) for st, d in _rows(box))
    assert rows["panel-files"][0] == "FAIL" and "use_trash" in rows["panel-files"][1], rows["panel-files"]


def test_the_files_row_fails_a_helper_that_differs(box):
    _healthy(box)
    (box.tmp / "lib" / "camera-box" / "obs_box_recordings.py").write_text("# old\n")
    rows = dict((d.split(")", 1)[0][1:], st) for st, d in _rows(box))
    assert rows["panel-files"] == "FAIL"


def test_the_row_count_is_eight():
    assert _bash("printf %s $OBS_BOX_PANEL_ROWS").stdout == "8"


def test_the_helper_is_std_only():
    text = HELPER.read_text()
    for mod in ("import requests", "import yaml", "import gi"):
        assert mod not in text
