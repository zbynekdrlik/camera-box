"""issue 1466 -- the live xfconf apply against the REAL xfconfd, not a fake.

`obs_box_xfconf.py apply` sets a box's declared properties through the desktop user's running xfconfd
with xfconf-query and reads each back. Its assumptions about xfconf-query (an existing property of
another value is overwritten by `-n -t T -s V`, the array form `-n -a -t T -s V ...`, the read-back
`Value is an array with N items:` + a blank line + one value per line, `-r -R` resets a subtree) are
measured here against the real daemon on a private session bus, and `check` grades the file the daemon
itself saves. CI installs the `xfconf` package (ci.yml, python-tests); a box without it FAILS by name,
never skips. Locally a scratch tree works too: OBS_BOX_TEST_XFCONF_ROOT=<dir with usr/bin/xfconf-query,
usr/lib/x86_64-linux-gnu/xfce4/xfconf/xfconfd and its libs> (`apt-get download xfconf libxfconf-0-3
libxfce4util7` + `dpkg-deb -x`).
"""
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
XFCONF = REPO / "scripts" / "obs_box_xfconf.py"
LIBDIR = "usr/lib/x86_64-linux-gnu"


def _tools():
    root = os.environ.get("OBS_BOX_TEST_XFCONF_ROOT", "/")
    query = Path(root) / "usr/bin/xfconf-query"
    daemon = Path(root) / LIBDIR / "xfce4/xfconf/xfconfd"
    missing = [str(p) for p in (query, daemon) if not os.access(p, os.X_OK)]
    if not shutil.which("dbus-daemon"):
        missing.append("dbus-daemon")
    assert not missing, ("the real-daemon test needs the xfconf package (CI installs it) -- missing: %s; "
                         "or point OBS_BOX_TEST_XFCONF_ROOT at an extracted tree" % ", ".join(missing))
    return root, query, daemon


class Daemon:
    def __init__(self, tmp):
        root, self.query, daemon = _tools()
        self.cfg = tmp / "cfg"
        self.cfg.mkdir()
        self.env = dict(os.environ, XDG_CONFIG_HOME=str(self.cfg),
                        PATH="%s:%s" % (self.query.parent, os.environ["PATH"]))
        if root != "/":
            self.env["LD_LIBRARY_PATH"] = str(Path(root) / LIBDIR)
        out = subprocess.run(["dbus-daemon", "--session", "--fork", "--print-address=1", "--print-pid=1",
                              "--address=unix:path=%s" % (tmp / "bus")],
                             capture_output=True, text=True, timeout=20, check=True).stdout.split()
        self.bus_pid = int(out[1])
        self.env["DBUS_SESSION_BUS_ADDRESS"] = out[0]
        self.xfconfd = subprocess.Popen([str(daemon)], env=self.env, stdout=subprocess.DEVNULL,
                                        stderr=subprocess.DEVNULL)
        for _ in range(50):
            if subprocess.run([str(self.query), "-l"], env=self.env, capture_output=True).returncode == 0:
                break
            time.sleep(0.1)

    def q(self, *args):
        return subprocess.run([str(self.query), *args], env=self.env, capture_output=True, text=True, timeout=20)

    def apply(self, channel, spec):
        return subprocess.run([sys.executable, str(XFCONF), "apply", channel, "-"], input=spec, env=self.env,
                              capture_output=True, text=True, timeout=120)

    def save_and_stop(self):
        self.xfconfd.send_signal(signal.SIGTERM)
        self.xfconfd.wait(timeout=20)
        return self.cfg / "xfce4/xfconf/xfce-perchannel-xml"

    def close(self):
        if self.xfconfd.poll() is None:
            self.xfconfd.kill()
            self.xfconfd.wait()
        os.kill(self.bus_pid, signal.SIGTERM)


KEYS = ("/commands/custom\treplace\n/commands/custom/override\tbool\ttrue\n"
        "/commands/custom/<Alt>Print\tstring\t/usr/bin/flameshot gui\n/commands/custom/Print\tstring\t/usr/bin/flameshot gui\n")
PANEL = ("/panels\treplace\n/plugins\treplace\n/configver\tint\t2\n/panels\tarray:int\t1\n"
         "/panels/panel-1/size\tuint\t30\n/panels/panel-1/position\tstring\tp=8;x=0;y=0\n"
         "/panels/panel-1/position-locked\tbool\ttrue\n/plugins/plugin-1\tstring\tapplicationsmenu\n"
         "/plugins/plugin-2\tstring\tlauncher\n/plugins/plugin-2/items\tarray:string\tobs-box-panel-power.desktop\n"
         "/panels/panel-1/plugin-ids\tarray:int\t1,2\n")
XFWM = "/general/use_compositing\tbool\tfalse\n"


def test_the_live_apply_against_the_real_xfconfd(tmp_path):
    d = Daemon(tmp_path)
    try:
        # values already there, one of them another value of the same key, one a key the spec drops
        assert d.q("-c", "xfwm4", "-p", "/general/use_compositing", "-n", "-t", "bool", "-s", "true").returncode == 0
        d.q("-c", "xfce4-keyboard-shortcuts", "-p", "/commands/custom/Print", "-n", "-t", "string", "-s", "xfce4-screenshooter")
        d.q("-c", "xfce4-keyboard-shortcuts", "-p", "/commands/custom/<Super>e", "-n", "-t", "string", "-s", "thunar")
        d.q("-c", "xfce4-panel", "-p", "/plugins/plugin-18", "-n", "-t", "string", "-s", "directorymenu")
        for channel, spec in (("xfce4-keyboard-shortcuts", KEYS), ("xfce4-panel", PANEL), ("xfwm4", XFWM)):
            r = d.apply(channel, spec)
            assert r.returncode == 0 and r.stdout.strip() == "ok", (channel, r.stdout, r.stderr)
        again = d.apply("xfce4-panel", PANEL)                     # idempotent
        assert again.returncode == 0, again.stderr
        assert d.q("-c", "xfce4-keyboard-shortcuts", "-p", "/commands/custom/<Super>e").returncode != 0
        assert d.q("-c", "xfce4-panel", "-p", "/plugins/plugin-18").returncode != 0
        assert d.q("-c", "xfwm4", "-p", "/general/use_compositing").stdout.strip() == "false"
        files = d.save_and_stop()
        for name, spec in (("xfce4-panel", PANEL), ("xfce4-keyboard-shortcuts", KEYS), ("xfwm4", XFWM)):
            r = subprocess.run([sys.executable, str(XFCONF), "check", str(files / ("%s.xml" % name)), "-"],
                               input=spec, capture_output=True, text=True)
            assert r.stdout.strip() == "ok", (name, r.stdout)
    finally:
        d.close()


def test_a_daemon_that_is_not_there_is_a_named_failure_never_a_traceback(tmp_path):
    _tools()
    env = dict(os.environ, DBUS_SESSION_BUS_ADDRESS="unix:path=%s" % (tmp_path / "no-bus"),
               PATH="%s:%s" % (Path(os.environ.get("OBS_BOX_TEST_XFCONF_ROOT", "/")) / "usr/bin", os.environ["PATH"]))
    root = os.environ.get("OBS_BOX_TEST_XFCONF_ROOT")
    if root:
        env["LD_LIBRARY_PATH"] = str(Path(root) / LIBDIR)
    r = subprocess.run([sys.executable, str(XFCONF), "apply", "xfwm4", "-"], input=XFWM, env=env,
                       capture_output=True, text=True, timeout=120)
    assert r.returncode == 1 and "Traceback" not in r.stderr and "xfwm4" in r.stderr, r.stderr
