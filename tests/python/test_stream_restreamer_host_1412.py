"""issue 1412 -- the stream ROLE hosts the restreamer's control-room Chrome + its hidden Xvfb (restreamer
issue 388, 13.10.2026): google-chrome-stable from Google's signed apt repo (the key accepted ONLY by its
pinned primary fingerprint) + xvfb, setup-strih step 16j, graded on verify item 10's stream branch. The
strih role never gets Chrome (production-only boxes, owner 24.9.2026)."""
import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
GOOD_FPR = "EB4C1BFD4F042F6DDDCCEC917721F63BD38B4796"


def _exec(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


class Box:
    def __init__(self, tmp, fpr=GOOD_FPR):
        self.bin = tmp / "bin"
        self.bin.mkdir()
        self.root = tmp / "root"
        (self.root / "etc/apt/keyrings").mkdir(parents=True)
        (self.root / "etc/apt/sources.list.d").mkdir(parents=True)
        self.log = tmp / "calls"
        self.state = tmp / "state"
        self.state.mkdir()
        _exec(self.bin / "curl", '#!/bin/bash\necho "curl $*" >> %s\nprintf "ASCII-ARMORED-KEY"\n' % self.log)
        _exec(self.bin / "gpg", '#!/bin/bash\necho "gpg $*" >> %s\n'
              'case "$*" in *--show-keys*) printf "pub:-:4096:1:x:1:::-:::scESC:::\\nfpr:::::::::%s:\\n";;'
              ' *--dearmor*) cat >/dev/null; printf "BINARY-KEYRING";; esac\n' % (self.log, fpr))
        _exec(self.bin / "apt-get", '#!/bin/bash\necho "apt-get $*" >> %s\n'
              'for p in "$@"; do case "$p" in google-chrome-stable|xvfb) touch %s/$p;; esac; done\n'
              % (self.log, self.state))
        _exec(self.bin / "dpkg-query", '#!/bin/bash\np="${@: -1}"\n[ -e %s/"$p" ] && printf "install ok installed"\n'
              'exit 0\n' % self.state)
        _exec(self.bin / "google-chrome", '#!/bin/bash\necho "Google Chrome 141.0.7390.54"\n')

    def env(self):
        return {"PATH": "%s:%s" % (self.bin, os.environ["PATH"]), "OBS_BOX_APT_ROOT": str(self.root)}

    def calls(self):
        return self.log.read_text() if self.log.exists() else ""


def _bash(body, box="stream-pp", env=None):
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\n' + body
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "h", str(FACTS), box, str(PROVISION)], capture_output=True,
                          text=True, env=e, timeout=60)


def test_step_16j_applies_to_the_stream_role_only():
    assert _bash("obs_box_step_applies 16j-restreamer").returncode == 0
    for box in ("strih-pp", "strih-snv"):
        r = _bash("obs_box_step_applies 16j-restreamer", box=box)
        assert r.returncode == 1 and "production" in r.stdout, (box, r.stdout)


def test_provision_adds_the_signed_repo_and_installs_both(tmp_path):
    b = Box(tmp_path)
    r = _bash("obs_box_stream_restreamer_host_provision", env=b.env())
    assert r.returncode == 0, r.stderr
    lst = (b.root / "etc/apt/sources.list.d/google-chrome.list").read_text()
    assert "signed-by=/etc/apt/keyrings/google-chrome.gpg" in lst and "dl.google.com/linux/chrome/deb/" in lst
    assert (b.root / "etc/apt/keyrings/google-chrome.gpg").read_text() == "BINARY-KEYRING"
    calls = b.calls()
    assert "https://dl.google.com/linux/linux_signing_key.pub" in calls
    assert "apt-get install -y google-chrome-stable xvfb" in calls
    assert calls.index("apt-get update") < calls.index("apt-get install")


def test_a_key_with_another_fingerprint_is_refused_and_nothing_is_written(tmp_path):
    b = Box(tmp_path, fpr="0" * 40)
    r = _bash("obs_box_stream_restreamer_host_provision", env=b.env())
    assert r.returncode != 0
    assert "fingerprint" in r.stderr
    assert not (b.root / "etc/apt/keyrings/google-chrome.gpg").exists()
    assert not (b.root / "etc/apt/sources.list.d/google-chrome.list").exists()
    assert "apt-get" not in b.calls()


def test_an_installed_host_is_left_alone(tmp_path):
    b = Box(tmp_path)
    _bash("obs_box_stream_restreamer_host_provision", env=b.env())
    b.log.unlink()
    r = _bash("obs_box_stream_restreamer_host_provision", env=b.env())
    assert r.returncode == 0, r.stderr
    assert "apt-get install" not in b.calls() and "curl" not in b.calls()


def _grade(b):
    r = _bash('ok(){ echo "OK $*"; }\nbad(){ echo "BAD $*"; }\nobs_box_stream_restreamer_host_grade', env=b.env())
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def test_the_grade_passes_an_installed_host_and_fails_a_missing_piece(tmp_path):
    b = Box(tmp_path)
    _bash("obs_box_stream_restreamer_host_provision", env=b.env())
    assert [ln.split()[0] for ln in _grade(b)] == ["OK"]
    (b.state / "xvfb").unlink()
    out = _grade(b)
    assert out[0].startswith("BAD") and "xvfb" in out[0]


def test_setup_and_verify_wire_step_16j():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert "obs_box_step_applies 16j-restreamer" in s and "obs_box_stream_restreamer_host_provision" in s
    assert "obs_box_stream_restreamer_host_grade" in v
    assert len(s.splitlines()) <= 1000 and len(v.splitlines()) <= 1000


CHROME_OWN_SOURCES = """### THIS FILE IS AUTOMATICALLY CONFIGURED ###
X-Repolib-Name: Google Chrome
Types: deb
URIs: https://dl.google.com/linux/chrome-stable/deb/
Suites: stable
Components: main
Architectures: amd64
Signed-By: /usr/share/keyrings/google-chrome.gpg
"""


def _chrome_took_over(b):
    """Live stream-pp 13.10.2026: Chrome's postinst deleted our .list and wrote its own deb822 .sources
    with its own keyring -- the repo Chrome itself keeps up to date."""
    (b.root / "etc/apt/sources.list.d/google-chrome.list").unlink()
    (b.root / "etc/apt/sources.list.d/google-chrome.sources").write_text(CHROME_OWN_SOURCES)
    (b.root / "usr/share/keyrings").mkdir(parents=True)
    (b.root / "usr/share/keyrings/google-chrome.gpg").write_text("CHROME-OWN-KEYRING")


def test_chromes_own_repo_file_counts_after_the_install(tmp_path):
    b = Box(tmp_path)
    _bash("obs_box_stream_restreamer_host_provision", env=b.env())
    _chrome_took_over(b)
    assert [ln.split()[0] for ln in _grade(b)] == ["OK"]
    b.log.unlink()
    r = _bash("obs_box_stream_restreamer_host_provision", env=b.env())
    assert r.returncode == 0, r.stderr
    assert "curl" not in b.calls() and "apt-get" not in b.calls()
    assert not (b.root / "etc/apt/sources.list.d/google-chrome.list").exists(), "never a second, duplicate source"
