"""Issue 1440 (phase 4 of the issue-1437 plan, design 6379965724 Approach 1): PipeWire out of the strih role.

The owner blacklisted PipeWire on the strih role (comment 6337295922). What this pins (Tier-0: pytest + bash,
fake apt-get / dpkg / dpkg-query / dpkg-deb / apt-mark / pgrep / systemctl on PATH, no box):
  * scripts/lib/strih-pipewire-purge.sh -- the DERIVED condition over the four real box fact files + hub
    TOMLs (strih-snv purges, strih-pp keeps PipeWire for its pipewire program_out, the stream role keeps its
    locked operator PipeWire); the apt pin for every architecture; the `apt-get -s` read that REFUSES a
    purge removing anything outside the declared list; the install order (pin, shim, keep libraries,
    read, purge, drop-ins); the verify rows;
  * strih_intercom_audio_dropin's `no-pipewire` render (no pipewire group, no PipeWire runtime env);
  * the browser sources: reroute_audio + muted through the seed (program_audio_vban.py + strih_scenes.py
    + the strih seed manifest) and the collection port;
  * the setup-strih step 12 / 13 + verify-strih item 9b wiring (the stream role branch untouched).
"""
import importlib.util
import json
import os
import pwd
import grp
import subprocess
import sys
from pathlib import Path

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_strih_stream_monitor_1436 import LEAKY, PROVISION, REPO  # noqa: E402

SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-pipewire-purge.sh"
BASELINE = SCRIPTS / "lib" / "obs-box-baseline.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
CONTROL = SCRIPTS / "strih-pipewire" / "camera-box-pipewire-shim.control"
PAV = SCRIPTS / "program_audio_vban.py"
PORT = SCRIPTS / "obs_collection_port.py"
ME = pwd.getpwuid(os.getuid()).pw_name
MYGROUP = grp.getgrgid(os.getgid()).gr_name
SEAMS = ("STRIH_PW_PURGE_PREF", "STRIH_PW_PURGE_OWNER", "STRIH_PW_PURGE_TOML", "STRIH_PW_PURGE_ASOUND",
         "STRIH_PW_PURGE_PROC", "STRIH_PW_PURGE_SCENES", "STRIH_PW_PURGE_HUB_DROPIN")
SERVER = ("pipewire", "pipewire-bin", "pipewire-pulse", "pipewire-alsa", "pipewire-audio", "pipewire-jack",
          "wireplumber", "pulseaudio", "gstreamer1.0-pipewire")
LEFTOVERS = ("pipewire/pipewire.conf.d/51-strih-quantum-1024.conf", "pipewire/pipewire.conf.d/strih-program.conf",
             "pipewire/pipewire.conf.d/strih-program-loopback.conf",
             "wireplumber/wireplumber.conf.d/51-minifuse-output-period.conf",
             "wireplumber/wireplumber.conf.d/51-strih-minifuse.conf")

# strih-snv, read-only 24.10.2026 (issue comment 6380038874): the box's installed set of these names.
LIVE_PKGS = [
    ("pipewire:amd64", "installed", "1.6.2-1ubuntu1.2"), ("pipewire-bin", "installed", "1.6.2-1ubuntu1.2"),
    ("pipewire-pulse", "installed", "1.6.2-1ubuntu1.2"), ("pipewire-alsa:amd64", "installed", "1.6.2-1ubuntu1.2"),
    ("pipewire-audio", "installed", "1.6.2-1ubuntu1.2"), ("wireplumber", "installed", "0.5.13-1ubuntu1"),
    ("gstreamer1.0-pipewire:amd64", "installed", "1.6.2-1ubuntu1.2"), ("pulseaudio", "not-installed", ""),
    ("libpulse0:amd64", "installed", "1:17.0+dfsg1-2ubuntu4"),
    ("libpipewire-0.3-0t64:amd64", "installed", "1.6.2-1ubuntu1.2"), ("rustdesk", "installed", "1.4.9"),
]
# The live `apt-get -s purge` read with the shim installed (rustdesk stays): the 22.10 inventory minus rustdesk.
SIM_OK = """Reading package lists...
0 upgraded, 0 newly installed, 12 to remove and 67 not upgraded.
Purg gnome-control-center [1:50.3-0ubuntu0.1]
Purg gnome-initial-setup [50.0-0ubuntu7.1]
Purg gnome-settings-daemon [50.0-1ubuntu1]
Purg gnome-snapshot [50.0-0ubuntu1]
Purg gstreamer1.0-pipewire [1.6.2-1ubuntu1.2]
Purg libcanberra-pulse [0.30-18ubuntu3]
Purg pipewire-audio [1.6.2-1ubuntu1.2]
Purg pipewire-pulse [1.6.2-1ubuntu1.2]
Purg pipewire-alsa [1.6.2-1ubuntu1.2]
Purg pipewire [1.6.2-1ubuntu1.2] [wireplumber:amd64 ]
Purg pipewire-bin [1.6.2-1ubuntu1.2] [wireplumber:amd64 ]
Purg wireplumber [0.5.13-1ubuntu1]
"""
SIM_RUSTDESK = SIM_OK + "Purg rustdesk [1.4.9]\n"


def _py_tool(body):
    return "#!%s\n%s" % (sys.executable, body)


FAKE_DPKG_QUERY = _py_tool(r'''
import os, sys
log = open(os.environ["FAKE_LOG"], "a")
log.write("dpkg-query " + " ".join(sys.argv[1:]) + "\n")
fmt = next((a[3:] for a in sys.argv[1:] if a.startswith("-f=")), "")
names = [a for a in sys.argv[1:] if not a.startswith("-")]
rows = [l.rstrip("\n").split("\t") for l in open(os.environ["FAKE_PKGS"]) if l.strip()]
miss = 0
for n in names:
    hit = [r for r in rows if r[0].split(":")[0] == n]
    if not hit:
        sys.stderr.write("dpkg-query: no packages found matching %s\n" % n); miss = 1
    for r in hit:
        if "binary:Package" in fmt:
            sys.stdout.write("%s\t%s\t%s\n" % (r[0], r[1], r[2]))
        else:
            sys.stdout.write("%s %s" % (r[1], r[2]))
sys.exit(miss)
''')
FAKE_DPKG = _py_tool(r'''
import os, sys
log = open(os.environ["FAKE_LOG"], "a")
log.write("dpkg " + " ".join(sys.argv[1:]) + "\n")
if sys.argv[1] == "--print-architecture":
    print("amd64")
elif sys.argv[1] == "--print-foreign-architectures":
    if os.environ.get("FAKE_FOREIGN", "i386"):
        print(os.environ.get("FAKE_FOREIGN", "i386"))
elif sys.argv[1] == "-i":
    ctl = open(sys.argv[2]).read()
    ver = [l.split(":", 1)[1].strip() for l in ctl.splitlines() if l.startswith("Version:")][0]
    assert "Provides: gstreamer1.0-pipewire" in ctl, ctl
    with open(os.environ["FAKE_PKGS"], "a") as fh:
        fh.write("camera-box-pipewire-shim\tinstalled\t%s\n" % ver)
''')
FAKE_DPKG_DEB = _py_tool(r'''
import os, shutil, sys
open(os.environ["FAKE_LOG"], "a").write("dpkg-deb " + " ".join(sys.argv[1:]) + "\n")
args = [a for a in sys.argv[1:] if not a.startswith("--")]
assert "--build" in sys.argv and "--root-owner-group" in sys.argv, sys.argv
shutil.copy(os.path.join(args[0], "DEBIAN", "control"), args[1])
''')
FAKE_APT_GET = _py_tool(r'''
import os, sys
open(os.environ["FAKE_LOG"], "a").write("apt-get " + " ".join(sys.argv[1:]) + "\n")
if "-s" in sys.argv:
    sys.stdout.write(open(os.environ["FAKE_SIM"]).read())
    sys.exit(int(os.environ.get("FAKE_SIM_RC", "0")))
if sys.argv[1:3] == ["install", "-y"] and sys.argv[3].endswith(".deb"):
    ctl = open(sys.argv[3]).read()
    ver = [l.split(":", 1)[1].strip() for l in ctl.splitlines() if l.startswith("Version:")][0]
    assert "Provides: gstreamer1.0-pipewire" in ctl and os.path.isabs(sys.argv[3]), (ctl, sys.argv)
    with open(os.environ["FAKE_PKGS"], "a") as fh:
        fh.write("camera-box-pipewire-shim\tinstalled\t%s\n" % ver)
    sys.exit(0)
if "purge" in sys.argv and "-y" in sys.argv:
    gone = {a.split(":")[0] for a in sys.argv[1:] if not a.startswith("-") and a != "purge"}
    rows = [l for l in open(os.environ["FAKE_PKGS"]) if l.strip()]
    if os.environ.get("FAKE_PURGE_LEAVES"):
        gone.discard(os.environ["FAKE_PURGE_LEAVES"])
    with open(os.environ["FAKE_PKGS"], "w") as fh:
        fh.writelines(l for l in rows if l.split("\t")[0].split(":")[0] not in gone)
''')
FAKE_APT_MARK = _py_tool(r'''
import os, sys
open(os.environ["FAKE_LOG"], "a").write("apt-mark " + " ".join(sys.argv[1:]) + "\n")
path = os.environ["FAKE_PKGS"] + ".manual"
have = set(open(path).read().split()) if os.path.exists(path) else set()
if sys.argv[1] == "manual":
    have |= {a.split(":")[0] for a in sys.argv[2:]}
    open(path, "w").write("\n".join(sorted(have)) + "\n")
elif sys.argv[1] == "showmanual":
    want = {a.split(":")[0] for a in sys.argv[2:]}
    sys.stdout.write("".join("%s\n" % n for n in sorted(have) if not want or n in want))
''')
FAKE_PGREP = _py_tool(r'''
import os, sys
open(os.environ["FAKE_LOG"], "a").write("pgrep " + " ".join(sys.argv[1:]) + "\n")
out = os.environ.get("FAKE_PGREP_OUT", "")
if out:
    print(out); sys.exit(0)
sys.exit(1)
''')
FAKE_SYSTEMCTL = _py_tool(r'''
import os, sys
if sys.argv[1:3] == ["is-active", "rustdesk"]:
    st = os.environ.get("FAKE_RUSTDESK", "active"); print(st); sys.exit(0 if st == "active" else 3)
sys.exit(0)
''')
FAKE_SCENES = _py_tool(r'''
import os, sys
assert "--browser-audio" in sys.argv, sys.argv
mode = os.environ.get("FAKE_SCENES_MODE", "ok")
if mode == "down":
    print("unreachable: ConnectionRefusedError: refused"); sys.exit(2)
print({"ok": "browser audio: OK (6 browser source(s) reroute their audio into OBS, muted)",
       "bad": "browser audio: 'Interkom' reroute_audio off"}[mode])
sys.exit(0 if mode == "ok" else 1)
''')


def _fakes(tmp_path, pkgs=None, sim=SIM_OK, extra=None):
    fb = tmp_path / "fakebin"
    fb.mkdir(exist_ok=True)
    for name, body in {"dpkg-query": FAKE_DPKG_QUERY, "dpkg": FAKE_DPKG, "dpkg-deb": FAKE_DPKG_DEB,
                       "apt-get": FAKE_APT_GET, "apt-mark": FAKE_APT_MARK, "pgrep": FAKE_PGREP,
                       "systemctl": FAKE_SYSTEMCTL}.items():
        (fb / name).write_text(body)
        (fb / name).chmod(0o755)
    (tmp_path / "pkgs").write_text("".join("%s\t%s\t%s\n" % r for r in (LIVE_PKGS if pkgs is None else pkgs)))
    (tmp_path / "sim").write_text(sim)
    (tmp_path / "log").write_text("")
    fs = tmp_path / "fake_scenes.py"
    fs.write_text(FAKE_SCENES)
    fs.chmod(0o755)
    e = {k: v for k, v in os.environ.items() if k not in LEAKY + SEAMS}
    e.update({"PATH": "%s:%s" % (fb, os.environ["PATH"]), "FAKE_LOG": str(tmp_path / "log"),
              "FAKE_PKGS": str(tmp_path / "pkgs"), "FAKE_SIM": str(tmp_path / "sim"),
              "STRIH_PW_PURGE_PREF": str(tmp_path / "etc" / "apt" / "preferences.d" / "camera-box-no-pipewire.pref"),
              "STRIH_PW_PURGE_OWNER": "%s:%s" % (ME, MYGROUP), "STRIH_PW_PURGE_SCENES": str(fs)})
    e.update(extra or {})
    return e


def _bash(body, env, *args, box="strih-snv"):
    script = ('set -euo pipefail\n. "%s"\n. "%s"\nfail() { echo "FAIL: $*" >&2; exit 1; }\n'
              'ok() { echo "OK $*"; }\nnote() { echo "NOTE $*"; }\nbad() { echo "BAD $*"; }\n'
              'strih_box_load "%s"\n%s' % (PROVISION, BASELINE, box, body))
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=env, cwd=str(REPO), timeout=120)


def _plain_env():
    return {k: v for k, v in os.environ.items() if k not in LEAKY + SEAMS}


def _log(tmp_path):
    return (tmp_path / "log").read_text().splitlines()


# --- 1. the derived condition over the four real boxes ------------------------------------------------------

@pytest.mark.parametrize("box,verdict,words", [
    ("strih-snv", "purge", ("STRIH_PROGRAM_VBAN_STREAMS=strih-obs", "intercom.strih-snv.toml")),
    ("strih-pp", "keep", ("pipewire", "program_out")),
    ("stream-pp", "keep", ("stream role", "1465")),
    ("stream-snv", "keep", ("stream role", "1465")),
])
def test_the_condition_is_derived_from_each_boxs_own_facts(box, verdict, words):
    r = _bash("strih_pipewire_purge_why || true", _plain_env(), box=box)
    assert r.returncode == 0, r.stderr
    line = r.stdout.strip()
    assert line.split("|")[0] == verdict, line
    for w in words:
        assert w in line, (w, line)


def test_applies_and_the_dropin_mode_follow_the_decision():
    for box, rc, mode in (("strih-snv", "0", "no-pipewire"), ("strih-pp", "1", "pipewire"),
                          ("stream-pp", "1", "pipewire")):
        r = _bash('rc=0; strih_pipewire_purge_applies >/dev/null || rc=$?; echo "rc=$rc"; '
                  'echo "mode=$(strih_pipewire_purge_dropin_mode)"', _plain_env(), box=box)
        assert r.returncode == 0, r.stderr
        assert "rc=%s" % rc in r.stdout and "mode=%s" % mode in r.stdout, (box, r.stdout)


def _toml(tmp_path, text, name="hub.toml"):
    p = tmp_path / name
    p.write_text(text)
    return p


def _decide(tmp_path, role, streams, toml):
    r = _bash('rc=0; strih_pipewire_purge_decision "$1" "$2" "$3" || rc=$?; echo "rc=$rc"', _plain_env(),
              role, streams, toml)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_a_commented_pipewire_adapter_is_not_a_participant(tmp_path):
    t = _toml(tmp_path, '# rollback: adapter = "pipewire" with pipewire_target\n[[participant]]\nname = "cutters"\n'
                        'adapter = "alsa"\n')
    out = _decide(tmp_path, "strih", "strih-obs", t)
    assert out.startswith("purge|") and "rc=0" in out, out


def test_a_pipewire_participant_keeps_pipewire_and_is_named(tmp_path):
    t = _toml(tmp_path, '[[participant]]\nname = "program_out"\nadapter = "pipewire"\n'
                        '[[participant]]\nname = "x"\nadapter = \'pipewire\'\n')
    out = _decide(tmp_path, "strih", "strih-obs", t)
    assert out.startswith("keep|") and "program_out, x" in out and "rc=1" in out, out


def test_no_program_vban_keeps_pipewire_without_reading_the_toml(tmp_path):
    out = _decide(tmp_path, "strih", "none", tmp_path / "absent.toml")
    assert out.startswith("keep|") and "STRIH_PROGRAM_VBAN_STREAMS=none" in out, out


@pytest.mark.parametrize("role,streams,toml", [("strih", "strih-obs", "absent"), ("strih", "strih-obs", "broken"),
                                               ("cutter", "strih-obs", "ok"), ("strih", "", "ok")])
def test_an_unreadable_input_is_unknown_never_a_purge(tmp_path, role, streams, toml):
    path = {"absent": tmp_path / "absent.toml", "broken": _toml(tmp_path, "[[participant\nname =", "broken.toml"),
            "ok": _toml(tmp_path, '[[participant]]\nname = "a"\nadapter = "alsa"\n', "ok.toml")}[toml]
    out = _decide(tmp_path, role, streams, path)
    assert out.startswith("unknown|") and "rc=2" in out, out


# --- 2. the pin and the `apt-get -s` read --------------------------------------------------------------------

def test_the_pin_names_every_server_package_for_every_architecture():
    r = _bash('strih_pipewire_purge_pin_text amd64 i386', _plain_env())
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    pkgs = [ln for ln in lines if ln.startswith("Package: ")]
    assert len(pkgs) == 1, lines
    names = pkgs[0][len("Package: "):].split()
    assert sorted(names) == sorted("%s:%s" % (p, a) for p in SERVER for a in ("amd64", "i386")), names
    assert "Pin: release *" in lines and "Pin-Priority: -1" in lines
    assert "libpulse0" not in pkgs[0] and "libpipewire" not in pkgs[0]
    assert all(ln.split(":")[0] in ("Explanation", "Package", "Pin", "Pin-Priority") for ln in lines), lines


def test_the_pin_needs_an_architecture():
    r = _bash('rc=0; strih_pipewire_purge_pin_text || rc=$?; echo "rc=$rc"', _plain_env())
    assert "rc=1" in r.stdout


def _sim(text):
    r = _bash('rc=0; strih_pipewire_purge_sim_verdict "$1" || rc=$?; echo "rc=$rc"', _plain_env(), text)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_the_live_read_with_the_shim_is_accepted():
    out = _sim(SIM_OK)
    assert out.startswith("ok 12 package(s)") and "rc=0" in out, out


@pytest.mark.parametrize("text,word", [
    (SIM_RUSTDESK, "rustdesk"),
    (SIM_OK + "Purg libpulse0:amd64 [1:17.0]\n", "libpulse0:amd64"),
    (SIM_OK + "Inst pulseaudio:i386 (1:17.0+dfsg1-2ubuntu4 Ubuntu:26.04/resolute [i386]) []\n", "pulseaudio:i386"),
    (SIM_OK + "E: Unable to locate package pipewire-jack\n", "Unable to locate"),
    ("Reading package lists...\n0 upgraded, 0 newly installed, 0 to remove.\n", "nothing to remove"),
])
def test_the_read_refuses_anything_outside_the_declared_list(text, word):
    out = _sim(text)
    assert out.startswith("refuse ") and word in out and "rc=1" in out, out


# --- 3. the install (setup-strih step 12 on a purge box) ----------------------------------------------------

def _home_with_leftovers(tmp_path):
    home = tmp_path / "home"
    for rel in LEFTOVERS:
        p = home / ".config" / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("# leftover\n")
    return home


def _install(tmp_path, env, home):
    return _bash('strih_pipewire_purge_install "$1" "$2" "$3"', env, REPO, home, ME)


def test_the_install_pins_builds_the_shim_reads_then_purges_and_removes_the_dropins(tmp_path):
    env = _fakes(tmp_path)
    home = _home_with_leftovers(tmp_path)
    r = _install(tmp_path, env, home)
    assert r.returncode == 0, (r.stdout, r.stderr)
    log = _log(tmp_path)
    pin = Path(env["STRIH_PW_PURGE_PREF"]).read_text()
    assert "pulseaudio:i386" in pin and "pipewire-pulse:i386" in pin and "Pin-Priority: -1" in pin
    i_deb = next(i for i, ln in enumerate(log) if ln.startswith("dpkg-deb --build --root-owner-group"))
    i_dpkg = next(i for i, ln in enumerate(log) if ln.startswith("apt-get install -y /") and ln.endswith(".deb"))
    assert not any(ln.startswith("dpkg -i") for ln in log), "dpkg -i ignores apt's lock wait"
    i_mark = next(i for i, ln in enumerate(log) if ln.startswith("apt-mark manual"))
    i_sim = next(i for i, ln in enumerate(log) if ln.startswith("apt-get -s purge"))
    i_purge = next(i for i, ln in enumerate(log) if ln.startswith("apt-get purge -y"))
    assert i_deb < i_dpkg < i_mark < i_sim < i_purge, log
    assert log[i_mark].split()[2:] == ["libpulse0:amd64", "libpipewire-0.3-0t64:amd64"], log[i_mark]
    purged = log[i_purge].split()[3:]
    assert purged == log[i_sim].split()[3:]
    assert sorted(p.split(":")[0] for p in purged) == sorted(p for p in SERVER if p != "pulseaudio" and p != "pipewire-jack")
    assert not any("rustdesk" in ln or "libpulse0" in ln.split("purge", 1)[-1] for ln in log if "purge" in ln), log
    left = (tmp_path / "pkgs").read_text()
    assert "rustdesk" in left and "libpulse0" in left and "libpipewire-0.3-0t64" in left
    assert "camera-box-pipewire-shim\tinstalled\t1.0" in left
    for rel in LEFTOVERS:
        assert not (home / ".config" / rel).exists(), rel
    assert "nothing to purge" not in r.stdout


def test_a_read_outside_the_declared_list_refuses_and_purges_nothing(tmp_path):
    env = _fakes(tmp_path, sim=SIM_RUSTDESK)
    home = _home_with_leftovers(tmp_path)
    r = _install(tmp_path, env, home)
    assert r.returncode != 0
    assert "REFUSED" in r.stderr and "rustdesk" in r.stderr, r.stderr
    assert not any(ln.startswith("apt-get purge -y") for ln in _log(tmp_path)), _log(tmp_path)
    for rel in LEFTOVERS:
        assert (home / ".config" / rel).exists(), rel
    assert "pipewire-bin" in (tmp_path / "pkgs").read_text()


def test_a_failing_read_refuses(tmp_path):
    env = _fakes(tmp_path, extra={"FAKE_SIM_RC": "100"})
    r = _install(tmp_path, env, _home_with_leftovers(tmp_path))
    assert r.returncode != 0 and "REFUSED" in r.stderr, r.stderr
    assert not any(ln.startswith("apt-get purge -y") for ln in _log(tmp_path))


def test_a_package_left_after_the_purge_fails(tmp_path):
    env = _fakes(tmp_path, extra={"FAKE_PURGE_LEAVES": "wireplumber"})
    r = _install(tmp_path, env, _home_with_leftovers(tmp_path))
    assert r.returncode != 0 and "still installed after the purge: wireplumber" in r.stderr, r.stderr


def test_a_rerun_changes_nothing_and_purges_nothing(tmp_path):
    env = _fakes(tmp_path)
    home = _home_with_leftovers(tmp_path)
    assert _install(tmp_path, env, home).returncode == 0
    (tmp_path / "log").write_text("")
    r = _install(tmp_path, env, home)
    assert r.returncode == 0, r.stderr
    log = _log(tmp_path)
    assert not any(ln.startswith(("apt-get", "dpkg -i", "dpkg-deb", "apt-mark manual")) for ln in log), log
    assert "unchanged" in r.stdout and "nothing to purge" in r.stdout, r.stdout


def test_running_pipewire_processes_are_named_never_stopped(tmp_path):
    env = _fakes(tmp_path, extra={"FAKE_PGREP_OUT": "2400 /usr/bin/pipewire"})
    r = _install(tmp_path, env, _home_with_leftovers(tmp_path))
    assert r.returncode == 0, r.stderr
    assert "reboot the box (owner approval)" in r.stdout
    assert not any(" stop" in ln or "kill" in ln for ln in _log(tmp_path))


def test_the_shim_control_file_provides_the_rustdesk_dependency():
    text = CONTROL.read_text()
    fields = dict(ln.split(":", 1) for ln in text.splitlines() if ln and not ln.startswith(" "))
    assert fields["Package"].strip() == "camera-box-pipewire-shim"
    assert fields["Provides"].strip() == "gstreamer1.0-pipewire"
    assert fields["Architecture"].strip() == "all"
    assert "Depends" not in fields and "Conflicts" not in fields


# --- 4. the hub drop-in ---------------------------------------------------------------------------------------

def test_the_no_pipewire_hub_dropin_drops_the_group_and_the_runtime():
    r = _bash('strih_intercom_audio_dropin newlevel 1000 no-pipewire', _plain_env())
    assert r.returncode == 0, r.stderr
    body = [ln for ln in r.stdout.splitlines() if ln and not ln.startswith("#")]
    for want in ("DynamicUser=no", "User=newlevel", "Group=newlevel", "SupplementaryGroups=audio render",
                 "ProtectHome=read-only"):
        assert want in body, (want, body)
    assert not any("pipewire" in ln.lower() or "XDG_RUNTIME_DIR" in ln or "After=" in ln for ln in body), body


def test_the_default_hub_dropin_is_unchanged():
    a = _bash('strih_intercom_audio_dropin newlevel 1000', _plain_env()).stdout
    b = _bash('strih_intercom_audio_dropin newlevel 1000 pipewire', _plain_env()).stdout
    assert a == b and "SupplementaryGroups=audio pipewire render" in a and "PIPEWIRE_RUNTIME_DIR" in a


def test_an_unknown_dropin_mode_refuses():
    r = _bash('rc=0; strih_intercom_audio_dropin newlevel 1000 nope >/dev/null || rc=$?; echo "rc=$rc"', _plain_env())
    assert "rc=1" in r.stdout


# --- 5. verify item 9b's rows --------------------------------------------------------------------------------

def _healthy_box(tmp_path, env):
    """A purged strih-snv: pin + shim, no server package, the card held by the hub, the drop-ins right."""
    pkgs = [r for r in LIVE_PKGS if r[0].split(":")[0] not in SERVER] + [("camera-box-pipewire-shim", "installed", "1.0")]
    (tmp_path / "pkgs").write_text("".join("%s\t%s\t%s\n" % r for r in pkgs))
    pref = Path(env["STRIH_PW_PURGE_PREF"])
    pref.parent.mkdir(parents=True, exist_ok=True)
    pref.write_text(_bash('strih_pipewire_purge_pin_text amd64 i386', _plain_env()).stdout)
    asound = tmp_path / "asound"
    for side, pid in (("p", 4242), ("c", 4243)):
        d = asound / "M4" / ("pcm0%s" % side) / "sub0"
        d.mkdir(parents=True)
        (d / "status").write_text("state: RUNNING\nowner_pid   : %d\ntrigger_time: 1.0\n" % pid)
    proc = tmp_path / "proc"
    for pid, comm in ((4242, "aplay"), (4243, "arecord")):
        (proc / str(pid)).mkdir(parents=True)
        (proc / str(pid) / "comm").write_text(comm + "\n")
    hub = tmp_path / "10-local-audio.conf"
    uid = os.getuid()
    hub.write_text(_bash('strih_intercom_audio_dropin "$1" "$2" no-pipewire', _plain_env(), ME, uid).stdout)
    env.update({"STRIH_PW_PURGE_ASOUND": str(asound), "STRIH_PW_PURGE_PROC": str(proc),
                "STRIH_PW_PURGE_HUB_DROPIN": str(hub)})
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    return home


def _rows(env, home, box="strih-snv"):
    r = _bash('strih_pipewire_purge_grade_rows "$1" "$2" "$3"', env, REPO, home, ME, box=box)
    assert r.returncode == 0, r.stderr
    out = {}
    end = None
    for ln in r.stdout.splitlines():
        st, text = ln.split("|", 1)
        if st == "END":
            end = int(text)
            continue
        out[text.split(")")[0] + ")"] = (st, text)
    assert end == len(out), (end, r.stdout)
    return out


def test_a_purged_box_passes_every_row(tmp_path):
    env = _fakes(tmp_path)
    home = _healthy_box(tmp_path, env)
    rows = _rows(env, home)
    assert len(rows) == 6, rows
    assert all(st == "OK" for st, _t in rows.values()), rows
    assert "M4 pcm0p=aplay" in rows["(pipewire-cards)"][1] and "M4 pcm0c=arecord" in rows["(pipewire-cards)"][1]


@pytest.mark.parametrize("break_it,row,word", [
    ("server", "(pipewire-packages)", "still installed: wireplumber"),
    ("pin", "(pipewire-packages)", "the pin"),
    ("shim", "(pipewire-packages)", "camera-box-pipewire-shim not installed"),
    ("libpulse", "(pipewire-packages)", "libpulse0 is gone"),
    ("process", "(pipewire-process)", "reboot the box"),
    ("rustdesk", "(pipewire-rustdesk)", "inactive"),
    ("card-pipewire", "(pipewire-cards)", "held by pipewire, not aplay"),
    ("card-closed", "(pipewire-cards)", "held by nothing"),
    ("card-gone", "(pipewire-cards)", "card M4 not on the box"),
    ("leftover", "(pipewire-dropins)", "51-strih-minifuse.conf is back"),
    ("hubdropin", "(pipewire-dropins)", "not the no-PipeWire render"),
    ("browser", "(browser-audio)", "reroute_audio off"),
])
def test_each_broken_fact_fails_its_row_by_name(tmp_path, break_it, row, word):
    env = _fakes(tmp_path)
    home = _healthy_box(tmp_path, env)
    pk = tmp_path / "pkgs"
    if break_it == "server":
        pk.write_text(pk.read_text() + "wireplumber\tinstalled\t0.5.13\n")
    elif break_it == "pin":
        Path(env["STRIH_PW_PURGE_PREF"]).write_text("Package: pulseaudio\nPin: release *\nPin-Priority: -1\n")
    elif break_it == "shim":
        pk.write_text("".join(ln for ln in pk.read_text().splitlines(True) if "shim" not in ln))
    elif break_it == "libpulse":
        pk.write_text("".join(ln for ln in pk.read_text().splitlines(True) if "libpulse0" not in ln))
    elif break_it == "process":
        env["FAKE_PGREP_OUT"] = "2400 /usr/bin/pipewire"
    elif break_it == "rustdesk":
        env["FAKE_RUSTDESK"] = "inactive"
    elif break_it == "card-pipewire":
        (Path(env["STRIH_PW_PURGE_PROC"]) / "4242" / "comm").write_text("pipewire\n")
    elif break_it == "card-closed":
        (Path(env["STRIH_PW_PURGE_ASOUND"]) / "M4" / "pcm0c" / "sub0" / "status").write_text("closed\n")
    elif break_it == "card-gone":
        env["STRIH_PW_PURGE_ASOUND"] = str(tmp_path / "no-asound")
    elif break_it == "leftover":
        p = home / ".config" / "wireplumber" / "wireplumber.conf.d" / "51-strih-minifuse.conf"
        p.parent.mkdir(parents=True)
        p.write_text("x\n")
    elif break_it == "hubdropin":
        Path(env["STRIH_PW_PURGE_HUB_DROPIN"]).write_text(
            _bash('strih_intercom_audio_dropin "$1" "$2"', _plain_env(), ME, os.getuid()).stdout)
    elif break_it == "browser":
        env["FAKE_SCENES_MODE"] = "bad"
    rows = _rows(env, home)
    st, text = rows[row]
    assert st == "FAIL" and word in text, (st, text)


def test_obs_down_is_a_note_for_the_browser_row(tmp_path):
    env = _fakes(tmp_path, extra={"FAKE_SCENES_MODE": "down"})
    rows = _rows(env, _healthy_box(tmp_path, env))
    assert rows["(browser-audio)"][0] == "NOTE"


def test_a_rustdesk_less_box_is_a_note(tmp_path):
    env = _fakes(tmp_path)
    home = _healthy_box(tmp_path, env)
    pk = tmp_path / "pkgs"
    pk.write_text("".join(ln for ln in pk.read_text().splitlines(True) if "rustdesk" not in ln))
    assert _rows(env, home)["(pipewire-rustdesk)"][0] == "NOTE"


def test_strih_pp_gets_one_note_naming_why_plus_the_browser_row(tmp_path):
    env = _fakes(tmp_path)
    rows = _rows(env, tmp_path, box="strih-pp")
    assert set(rows) == {"(pipewire-purge)", "(browser-audio)"}, rows
    st, text = rows["(pipewire-purge)"]
    assert st == "NOTE" and "program_out" in text, text
    assert not any(ln.startswith(("dpkg-query", "pgrep")) for ln in _log(tmp_path)), _log(tmp_path)


@pytest.mark.parametrize("box", ["stream-pp", "stream-snv"])
def test_the_stream_role_gets_one_note_and_nothing_else(tmp_path, box):
    env = _fakes(tmp_path)
    rows = _rows(env, tmp_path, box=box)
    assert list(rows) == ["(pipewire-purge)"], rows
    st, text = rows["(pipewire-purge)"]
    assert st == "NOTE" and "1465" in text, text


def test_the_report_routes_rows_and_counts_them(tmp_path):
    env = _fakes(tmp_path)
    home = _healthy_box(tmp_path, env)
    r = _bash('strih_pipewire_purge_grade_report "$1" "$2" "$3" && echo REPORT-OK', env, REPO, home, ME)
    assert r.returncode == 0 and "REPORT-OK" in r.stdout and "BAD" not in r.stdout, (r.stdout, r.stderr)
    assert r.stdout.count("OK (") == 6, r.stdout


# --- 6. the wiring + the line budgets --------------------------------------------------------------------------

def _section(text, start, end):
    a = text.index(start)
    return text[a:text.index(end, a)]


def test_setup_step12_purges_only_after_the_stream_branch_and_before_the_strih_graph():
    s = _section(SETUP.read_text(), "\nstep 12 \"", "\nstep 13 \"")
    gate = s.index("obs_box_step_applies 12-strih-audio")
    stream = s.index('stream_operator_audio_install "${HERE}/.." "$USER_HOME" "$DESKTOP_USER"')
    purge = s.index("strih_pipewire_purge_applies")
    install = s.index('strih_pipewire_purge_install "${HERE}/.." "$USER_HOME" "$DESKTOP_USER"')
    graph = s.index("strih_pipewire_program_sink_conf")
    assert gate < stream < purge < install < graph
    assert s[purge - 30:purge].rstrip().endswith("elif PW_WHY=\"$(") or "elif" in s[stream:purge], s[stream:purge]


def test_setup_step13_renders_the_hub_dropin_by_the_derived_mode():
    s = _section(SETUP.read_text(), "\nstep 13 \"", "\nstep 14 \"")
    line = next(ln for ln in s.splitlines() if "strih_intercom_audio_dropin" in ln)
    assert '"$(strih_pipewire_purge_dropin_mode)"' in line and "10-local-audio.conf" in line, line


def test_verify_item_9b_grades_the_purge_and_keeps_the_strih_and_stream_branches():
    v = _section(VERIFY.read_text(), "# 9b) ", "# 10) NVENC")
    report = v.index('strih_pipewire_purge_grade_report "${HERE}/.." "$USER_HOME"')
    gate = v.index("obs_box_step_applies 12-strih-audio")
    stream = v.index("stream_operator_audio_grade_report")
    skip = v.index("elif strih_pipewire_purge_applies")
    quantum = v.index("51-strih-quantum-1024.conf")
    assert report < gate < stream < skip < quantum


def test_the_provisioning_lib_sources_the_purge_lib():
    s = PROVISION.read_text()
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-pipewire-purge.sh"' in s
    assert s.index("stream-operator-audio.sh") < s.index("strih-pipewire-purge.sh")


def test_both_scripts_stay_within_their_line_budget():
    assert len(SETUP.read_text().splitlines()) <= 1000
    assert len(VERIFY.read_text().splitlines()) <= 1000


# --- 7. the browser sources: reroute + mute ------------------------------------------------------------------

def _pav():
    spec = importlib.util.spec_from_file_location("program_audio_vban_1440", PAV)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


class FakeObs:
    """An OBS with browser sources {name: (settings, muted)} plus one non-browser input."""

    def __init__(self, browsers, refuse=()):
        self.browsers = {n: (dict(s), m) for n, (s, m) in browsers.items()}
        self.refuse = set(refuse)
        self.calls = []

    def req(self, t, d=None, ignore_err=False):
        self.calls.append((t, d))
        if t == "GetInputList":
            kind = (d or {}).get("inputKind")
            items = [{"inputName": n, "inputKind": "browser_source"} for n in self.browsers]
            items.append({"inputName": "NDI cam2", "inputKind": "ndi_source"})
            return {"inputs": [i for i in items if kind in (None, i["inputKind"])]}
        if t == "GetInputSettings":
            return {"inputSettings": dict(self.browsers[d["inputName"]][0])}
        if t == "GetInputMute":
            return {"inputMuted": self.browsers[d["inputName"]][1]}
        if t == "SetInputSettings" and d["inputName"] not in self.refuse:
            s, m = self.browsers[d["inputName"]]
            s.update(d["inputSettings"])
            self.browsers[d["inputName"]] = (s, m)
        if t == "SetInputMute" and d["inputName"] not in self.refuse:
            s, _m = self.browsers[d["inputName"]]
            self.browsers[d["inputName"]] = (s, d["inputMuted"])
        return {}

    def sets(self):
        return [(t, d["inputName"]) for t, d in self.calls if t.startswith("Set")]


def test_the_seed_reroutes_and_mutes_every_browser_source_and_reads_it_back():
    obs = FakeObs({"Interkom": ({"url": "http://x"}, False), "Ableset": ({"reroute_audio": True}, True),
                   "Odpocet": ({"reroute_audio": False}, True)})
    status = _pav().mute_browser_sources(obs)
    assert all(st.get("reroute_audio") is True and muted for st, muted in obs.browsers.values()), obs.browsers
    assert ("SetInputMute", "Ableset") not in obs.sets() and ("SetInputSettings", "Ableset") not in obs.sets()
    assert ("SetInputSettings", "Interkom") in obs.sets() and ("SetInputMute", "Interkom") in obs.sets()
    assert ("SetInputMute", "Odpocet") not in obs.sets() and ("SetInputSettings", "Odpocet") in obs.sets()
    assert not any(n == "NDI cam2" for _t, n in obs.sets())
    assert status.startswith("3 browser source(s) reroute their audio into OBS, muted"), status
    assert "Interkom" in status and "Odpocet" in status and "FAILED" not in status


def test_a_refused_set_is_named_by_its_read_back():
    obs = FakeObs({"Interkom": ({}, False)}, refuse=("Interkom",))
    status = _pav().mute_browser_sources(obs)
    assert "FAILED" in status and "Interkom" in status, status


def test_a_healthy_set_changes_nothing():
    obs = FakeObs({"Interkom": ({"reroute_audio": True}, True)})
    status = _pav().mute_browser_sources(obs)
    assert obs.sets() == [] and status == "1 browser source(s) reroute their audio into OBS, muted", status


def test_the_read_only_report():
    m = _pav()
    assert m.browser_audio_report(FakeObs({})) == "browser audio: OK (no browser source)"
    ok = m.browser_audio_report(FakeObs({"A": ({"reroute_audio": True}, True)}))
    assert ok == "browser audio: OK (1 browser source(s) reroute their audio into OBS, muted)", ok
    bad = m.browser_audio_report(FakeObs({"A": ({}, True), "B": ({"reroute_audio": True}, False)}))
    assert bad.startswith("browser audio: ") and "'A' reroute_audio off" in bad and "'B' not muted" in bad, bad
    assert not bad.startswith("browser audio: OK")


def test_the_manifest_rule_is_lenient():
    m = _pav()
    assert m.parse_browser_audio_rule('{"browser_audio": "reroute-mute"}') == "reroute-mute"
    for text in ("{}", "not json", '{"browser_audio": "yes"}', "[]"):
        assert m.parse_browser_audio_rule(text) is None, text


@pytest.mark.parametrize("box,want", [("strih-snv", "reroute-mute"), ("strih-pp", "reroute-mute"),
                                      ("stream-pp", None), ("stream-snv", None)])
def test_the_strih_manifests_carry_the_rule_and_the_stream_ones_do_not(box, want):
    r = _bash("obs_box_seed_manifest_json", _plain_env(), box=box)
    assert r.returncode == 0, r.stderr
    json.loads(r.stdout)
    assert _pav().parse_browser_audio_rule(r.stdout) == want


def test_strih_scenes_wires_the_rule_into_the_seed_and_a_read_only_mode():
    s = (SCRIPTS / "strih_scenes.py").read_text()
    assert "browser_audio=parse_browser_audio_rule(manifest_text)" in s
    assert 'result["browser audio"] = mute_browser_sources(obs)' in s
    assert '"--browser-audio"' in s and "browser_audio_report(obs)" in s


def _port():
    spec = importlib.util.spec_from_file_location("obs_collection_port_1440", PORT)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


@pytest.mark.parametrize("preset", ["strih-pp", "strih-snv"])
def test_the_strih_port_reroutes_and_mutes_every_browser_source(preset):
    m = _port()
    coll = {"sources": [
        {"name": "Interkom", "id": "browser_source", "settings": {"url": "http://strih.lan:8790/"}, "muted": False},
        {"name": "Ableset", "id": "browser_source", "settings": {"reroute_audio": True}, "muted": True},
        {"name": "NDI cam1", "id": "ndi_source", "settings": {}, "muted": False},
        {"name": "ASIO zvuk", "id": "pulse_input_capture", "settings": {}, "muted": False}]}
    out, rep = m.port(coll, m.preset_options(preset))
    by = {s["name"]: s for s in out["sources"]}
    assert by["Interkom"]["settings"]["reroute_audio"] is True and by["Interkom"]["muted"] is True
    assert by["Ableset"]["settings"]["reroute_audio"] is True and by["Ableset"]["muted"] is True
    assert by["NDI cam1"]["muted"] is False and "reroute_audio" not in by["NDI cam1"]["settings"]
    assert sum(1 for ln in rep if ln.startswith("AUDIO browser")) == 1, rep
    assert coll["sources"][0]["muted"] is False, "the input collection is never modified"


def test_the_stream_port_leaves_browser_audio_alone():
    m = _port()
    coll = {"sources": [{"name": "PRO", "id": "scene", "settings": {"items": [{"name": "YT"}, {"name": "Zvuk z mixu"}]}},
                        {"name": "YT", "id": "browser_source", "settings": {}, "muted": False},
                        {"name": "Zvuk z mixu", "id": "wasapi_input_capture", "settings": {}, "muted": False}]}
    out, _rep = m.port(coll, m.preset_options("stream-pp"))
    yt = next(s for s in out["sources"] if s["name"] == "YT")
    assert yt["muted"] is False and "reroute_audio" not in yt["settings"]


# --- 8. the fixed HDMI output's audio on a box with no sound server (strih-snv's vk-direct HDMI-0) ----------
# After the purge `wireplumber --version` answers nothing: the issue-1424 WirePlumber deny rule has nothing to
# deny, and verify item 4c must not FAIL `wp-unknown` on the box the purge made correct.

DRM_AUDIO = SCRIPTS / "lib" / "drm-output-audio.sh"
NO_SERVER = {"DRM_OUTPUT_AUDIO_SERVER_BINS": "camera-box-no-such-server-1440 camera-box-no-such-pipewire-1440"}


def _drm_audio(body, *args, extra=None):
    e = _plain_env()
    e.update(extra or {})
    script = 'set -euo pipefail\n. "%s"\nok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }; note() { echo "NOTE $1"; }\n%s' % (
        DRM_AUDIO, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, timeout=60)


def test_no_sound_server_writes_no_wireplumber_rule(tmp_path):
    root = tmp_path / "root"
    r = _drm_audio('drm_output_audio_wp_provision "$1"', root, extra=NO_SERVER)
    assert r.returncode == 0, r.stderr
    assert "no sound server" in r.stdout and "WARNING" not in r.stderr, (r.stdout, r.stderr)
    assert not (root / "etc").exists()


def test_no_sound_server_grades_the_hdmi_leg_on_its_own_line(tmp_path):
    from test_drm_output_audio_1424 import OPEN  # noqa: E402
    log = tmp_path / "obs.txt"
    log.write_text(OPEN + "\n")
    r = _drm_audio('drm_output_audio_verify_report "$1" "$2" "$3" "$4"', tmp_path, ME, log, tmp_path / "root",
                   extra=NO_SERVER)
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("PASS HDMI audio:") and "no sound server" in r.stdout, r.stdout
    log.write_text("info: drm-output audio: NO AUDIO on 'HDMI-0' -- no ELD\n")
    r = _drm_audio('drm_output_audio_verify_report "$1" "$2" "$3" "$4"', tmp_path, ME, log, tmp_path / "root",
                   extra=NO_SERVER)
    assert r.stdout.startswith("FAIL HDMI audio: the module gave up"), r.stdout


def test_the_verdict_takes_none_as_no_rule_needed():
    r = _drm_audio('rc=0; drm_output_audio_verdict none 0 open || rc=$?; echo " rc=$rc"')
    assert r.stdout.strip() == "ok rc=0", r.stdout


# --- 9. review round 1 --------------------------------------------------------------------------------------

def test_a_server_package_left_in_config_files_state_is_purged_too(tmp_path):
    env = _fakes(tmp_path, pkgs=LIVE_PKGS + [("pipewire-jack", "config-files", "1.6.2-1ubuntu1.2")])
    r = _install(tmp_path, env, _home_with_leftovers(tmp_path))
    assert r.returncode == 0, r.stderr
    purge = next(ln for ln in _log(tmp_path) if ln.startswith("apt-get purge -y"))
    assert "pipewire-jack" in purge.split(), purge


def _step12(setup_text):
    s = _section(setup_text, "\nstep 12 \"", "\nstep 13 \"")
    a = s.index('if ! WHY="$(obs_box_step_applies 12-strih-audio)"')
    b = s.index("\nelse\n", a)
    return s[a:b] + "\nelse\n  echo STRIH-GRAPH\nfi\n"


@pytest.mark.parametrize("rc,want", [("0", "INSTALL"), ("1", "STRIH-GRAPH"), ("2", "FAILED")])
def test_setup_step12_refuses_when_the_purge_cannot_be_decided(rc, want):
    script = ("set -euo pipefail\n"
              "obs_box_step_applies() { return 0; }\n"
              "strih_pipewire_purge_applies() { echo 'why it says'; return %s; }\n"
              "strih_pipewire_purge_install() { echo INSTALL; }\n"
              "stream_operator_audio_install() { echo STREAM; }\n"
              "fail() { echo \"FAILED: $*\"; exit 9; }\n"
              "HERE=. USER_HOME=/h DESKTOP_USER=u\n" % rc) + _step12(SETUP.read_text())
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30)
    assert want in r.stdout, (r.stdout, r.stderr)
    if rc == "2":
        assert r.returncode == 9 and "cannot decide" in r.stdout and "why it says" in r.stdout, r.stdout
        assert "STRIH-GRAPH" not in r.stdout
