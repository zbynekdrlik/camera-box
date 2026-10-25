"""issue 1436 -- the stream operator's Audio Monitor plugin on the Linux stream boxes.

Owner 25.10.2026: on the Windows stream box the operator had exeldro's Audio Monitor plugin (the program audio on
its own fader in the headphones); the Linux stream-snv / stream-pp had none. ROZHODNUTE (25.10.2026 08:45): it is
installed after the service, stream role only, through setup + verify. These tests run the real install against a
locally built stand-in .deb (curl stubbed, the pins overridden by redefining the pin functions after sourcing) and
the real grade rows.
"""
import hashlib
import os
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
LIB = SCRIPTS / "lib" / "obs-audio-monitor.sh"
SO_BYTES = b"\x7fELF fake audio-monitor plugin for the test\n"


def _sha(b):
    return hashlib.sha256(b).hexdigest()


def _deb(tmp, so=SO_BYTES):
    root = tmp / "debroot"
    (root / "DEBIAN").mkdir(parents=True)
    (root / "DEBIAN" / "control").write_text("Package: audio-monitor\nVersion: 0.10.1\nArchitecture: amd64\n"
                                            "Maintainer: test <t@t>\nDescription: stand-in\n")
    p = root / "usr/lib/x86_64-linux-gnu/obs-plugins"
    p.mkdir(parents=True)
    (p / "audio-monitor.so").write_bytes(so)
    loc = root / "usr/share/obs/obs-plugins/audio-monitor/locale"
    loc.mkdir(parents=True)
    (loc / "en-US.ini").write_text("AudioMonitor=\"Audio Monitor\"\n")
    deb = tmp / "am.deb"
    subprocess.run(["dpkg-deb", "--root-owner-group", "-b", str(root), str(deb)], check=True, capture_output=True)
    return deb


class Box:
    def __init__(self, tmp, so=SO_BYTES, want_so=None, want_deb=None):
        self.tmp = tmp
        self.deb = _deb(tmp, so)
        self.bin = tmp / "bin"
        self.bin.mkdir()
        self.calls = tmp / "curl.log"
        (self.bin / "curl").write_text('#!/bin/bash\necho "curl $*" >> %s\nout=""\nwhile [ $# -gt 0 ]; do '
                                       '[ "$1" = -o ] && { out="$2"; shift; }; shift; done\ncp %s "$out"\n'
                                       % (self.calls, self.deb))
        (self.bin / "curl").chmod(0o755)
        self.lib = tmp / "lib"
        self.share = tmp / "share"
        self.logs = tmp / "logs"
        self.logs.mkdir()
        self.want_so = want_so or _sha(SO_BYTES)
        self.want_deb = want_deb or _sha(self.deb.read_bytes())

    def run(self, body, box="stream-pp"):
        pins = ('obs_audio_monitor_so_sha256() { printf %s; }\nobs_audio_monitor_deb_sha256() { printf %s; }\n'
                % (self.want_so, self.want_deb))
        script = ('set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\n' + pins +
                  'ok() { echo "OK $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n' + body)
        env = dict(os.environ, PATH="%s:%s" % (self.bin, os.environ["PATH"]), STRIH_LIBDIR=str(self.lib),
                   STRIH_SHAREDIR=str(self.share), STRIH_AUDIO_MONITOR_LOG_DIR=str(self.logs), TMPDIR=str(self.tmp))
        return subprocess.run(["bash", "-c", script, "h", str(FACTS), box, str(PROVISION)],
                              capture_output=True, text=True, env=env, timeout=60)

    def install(self, box="stream-pp"):
        return self.run('obs_audio_monitor_install "%s" "%s"' % (self.lib, self.share), box)

    def rows(self, box="stream-pp"):
        r = self.run("obs_audio_monitor_grade_report", box)
        assert r.returncode == 0, r.stderr
        return r.stdout.splitlines()

    @property
    def so(self):
        return self.lib / "obs-plugins" / "audio-monitor.so"


def test_the_stream_role_installs_the_pinned_plugin_and_its_locale(tmp_path):
    box = Box(tmp_path)
    r = box.install()
    assert r.returncode == 0, r.stderr
    assert box.so.read_bytes() == SO_BYTES
    assert (box.share / "obs/obs-plugins/audio-monitor/locale/en-US.ini").is_file()
    assert "installed ->" in r.stdout


def test_a_second_run_downloads_nothing(tmp_path):
    box = Box(tmp_path)
    box.install()
    box.calls.unlink()
    r = box.install()
    assert r.returncode == 0 and "already installed" in r.stdout
    assert not box.calls.exists()


def test_the_strih_role_never_gets_it(tmp_path):
    box = Box(tmp_path)
    r = box.install(box="strih-snv")
    assert r.returncode == 0 and "strih role" in r.stdout
    assert not box.so.exists() and not box.calls.exists()
    assert box.rows(box="strih-snv")[0].startswith("NOTE")


def test_a_deb_hash_mismatch_installs_nothing(tmp_path):
    box = Box(tmp_path, want_deb="0" * 64)
    r = box.install()
    assert r.returncode != 0 and ".deb sha256 mismatch" in r.stderr
    assert not box.so.exists()


def test_a_plugin_hash_mismatch_installs_nothing(tmp_path):
    box = Box(tmp_path, want_so="1" * 64)
    r = box.install()
    assert r.returncode != 0 and "plugin sha256 mismatch" in r.stderr
    assert not box.so.exists()


def test_the_grade_rows(tmp_path):
    box = Box(tmp_path)
    assert box.rows()[0].startswith("FAIL") and "missing" in box.rows()[0]
    box.install()
    log = box.logs / "2026-10-25 18-00-00.txt"
    log.write_text("18:00:00.000: [Audio Monitor] loaded version 0.10.1\n")
    old = time.time() - 3600
    os.utime(log, (old, old))
    rows = box.rows()
    assert rows[0].startswith("OK") and rows[1].startswith("NOTE"), rows   # OBS not restarted since
    os.utime(log, None)
    assert box.rows()[1].startswith("OK"), box.rows()
    log.write_text("18:00:00.000: other plugin lines only\n")
    assert box.rows()[1].startswith("FAIL"), box.rows()
    box.so.write_bytes(b"other build")
    assert box.rows()[0].startswith("FAIL") and "not the pinned" in box.rows()[0]


def test_setup_and_verify_wire_it_within_their_budget():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert len(s.splitlines()) <= 1000 and len(v.splitlines()) <= 1000
    step = s[s.index('step "4c"'):s.index("step 5 ")]
    assert 'obs_audio_monitor_install /usr/lib/x86_64-linux-gnu /usr/share || fail' in step
    assert v.count("obs_audio_monitor_grade_report") == 1


def test_the_pins_are_the_upstream_release_digest():
    r = subprocess.run(["bash", "-c", '. "$1"; obs_audio_monitor_deb_url; echo; obs_audio_monitor_deb_sha256', "h", str(LIB)],
                       capture_output=True, text=True, timeout=30)
    url, sha = r.stdout.split()
    assert url.endswith("/0.10.1/audio-monitor-0.10.1-x86_64-linux-gnu.deb")
    # the GitHub release digest of that asset, read 25.10.2026
    assert sha == "0cd326b90f5b0fafc812b5c406edf802118f12ccf5e3dd309a3966fe190cc886"
