#!/usr/bin/env python3
"""The recordings item of the Linux OBS boxes' panel (issue 1458).

Owner 21.10.2026: "este jedna vec ktoru chcem aby si spravil aby taktiez v liste na linuxoch boli file
explorer aby vedeli vymazat nahravky ktore robi obs ked uz je ich tam vela". The panel's `Nahrávky`
launcher runs this file:

    print               prints the ACTIVE OBS profile's record folder
    open                starts the file manager (pcmanfm) in that folder
    seed-libfm CONF     sets pcmanfm's [config] use_trash=0 + confirm_del=1 in CONF (other keys kept,
                        written only on a difference); prints `written` / `unchanged`
    check-libfm CONF    rc 0 when CONF carries use_trash=0, else rc 1 + one line naming why

The folder is resolved the way OBS resolves it, at click time, so a profile the operator changes in OBS
settings is followed: [Basic] ProfileDir of user.ini (then global.ini, then the display name Profile),
that profile's basic.ini, Advanced = [AdvOut] RecFilePath (FFFilePath when RecType=FFmpeg), Simple =
[SimpleOutput] FilePath. An empty or missing folder falls back to OBS's own default, ~/Videos when it
exists, else the home dir (stream-pp's profile, 21.10.2026, carries no RecFilePath at all). The same
resolution as scripts/strih-recordings-retention.sh `rr_obs_profile_record_dir`, which refuses instead
of falling back because it feeds a delete.

use_trash=0: the trash of a folder on another disk lives on that disk, so a trashed recording would
keep its space. confirm_del=1 keeps the confirmation before every delete.
"""

import configparser
import os
import sys
from pathlib import Path

LIBFM_KEYS = (("use_trash", "0"), ("confirm_del", "1"))


def _ini(path):
    """A case-preserving, lenient parser over PATH (utf-8-sig, CRLF ok); None when it cannot be read."""
    cp = configparser.RawConfigParser(strict=False, interpolation=None)
    cp.optionxform = str
    try:
        with open(path, encoding="utf-8-sig") as f:
            cp.read_file(f)
    except (OSError, configparser.Error, UnicodeDecodeError):
        return None
    return cp


def _get(cp, section, key):
    if cp is None or not cp.has_section(section):
        return ""
    return cp.get(section, key, fallback="").strip()


def obs_default_folder(home):
    """OBS's own default record folder: ~/Videos when it exists, else the home dir."""
    videos = home / "Videos"
    return videos if videos.is_dir() else home


def profile_record_path(cfg):
    """The record path the active profile under CFG (~/.config/obs-studio) names, '' when none."""
    pdir = ""
    for key in ("ProfileDir", "Profile"):
        for name in ("user.ini", "global.ini"):
            pdir = _get(_ini(cfg / name), "Basic", key)
            if pdir:
                break
        if pdir:
            break
    if not pdir:
        return ""
    basic = _ini(cfg / "basic" / "profiles" / pdir / "basic.ini")
    if _get(basic, "Output", "Mode") == "Advanced":
        key = "FFFilePath" if _get(basic, "AdvOut", "RecType") == "FFmpeg" else "RecFilePath"
        return _get(basic, "AdvOut", key)
    return _get(basic, "SimpleOutput", "FilePath")


def record_folder(home):
    """The folder the panel opens: the profile's record path when it is a folder, else OBS's default."""
    path = profile_record_path(home / ".config" / "obs-studio")
    if path:
        p = Path(os.path.expanduser(path))
        if p.is_dir():
            return p
        print("obs_box_recordings: the OBS record path %s is not a folder -- opening OBS's default" % path,
              file=sys.stderr)
    return obs_default_folder(home)


def seed_libfm(conf):
    """Set LIBFM_KEYS in CONF's [config]; 'written' or 'unchanged'. Raises OSError on a failed write."""
    cp = _ini(conf) if conf.exists() else None
    if cp is None:
        cp = configparser.RawConfigParser(strict=False, interpolation=None)
        cp.optionxform = str
    if not cp.has_section("config"):
        cp.add_section("config")
    if all(cp.get("config", k, fallback=None) == v for k, v in LIBFM_KEYS) and conf.exists():
        return "unchanged"
    for k, v in LIBFM_KEYS:
        cp.set("config", k, v)
    conf.parent.mkdir(parents=True, exist_ok=True)
    tmp = conf.with_name(conf.name + ".obs-box-new")
    with open(tmp, "w", encoding="utf-8") as f:
        cp.write(f, space_around_delimiters=False)
    os.replace(tmp, conf)
    return "written"


def check_libfm(conf):
    """None when CONF carries use_trash=0, else the reason."""
    if not conf.exists():
        return "%s missing" % conf
    cp = _ini(conf)
    if cp is None:
        return "%s unreadable" % conf
    val = cp.get("config", "use_trash", fallback=None)
    if val != "0":
        return "%s use_trash=%s (want 0: a trashed recording keeps its disk space)" % (conf, val)
    return None


def main(argv):
    home = Path(os.path.expanduser("~"))
    if len(argv) == 2 and argv[1] == "print":
        print(record_folder(home))
        return 0
    if len(argv) == 2 and argv[1] == "open":
        folder = str(record_folder(home))
        fm = os.environ.get("OBS_BOX_RECORDINGS_FM") or "pcmanfm"
        try:
            os.execvp(fm, [fm, folder])
        except OSError as exc:
            print("obs_box_recordings: cannot start %s: %s" % (fm, exc), file=sys.stderr)
            return 1
    if len(argv) == 3 and argv[1] == "seed-libfm":
        try:
            print(seed_libfm(Path(argv[2])))
        except OSError as exc:
            print("obs_box_recordings: cannot write %s: %s" % (argv[2], exc), file=sys.stderr)
            return 1
        return 0
    if len(argv) == 3 and argv[1] == "check-libfm":
        why = check_libfm(Path(argv[2]))
        if why:
            print(why)
            return 1
        print("%s use_trash=0" % argv[2])
        return 0
    print("usage: obs_box_recordings.py print|open|seed-libfm CONF|check-libfm CONF", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
