"""issue 1448 part 2 -- setup installs Discord on the stream-role Linux boxes, verify grades it.

Discord on stream-pp was a hand install (`apt-get install ./discord.deb`, 14.10.2026); nothing in
setup-strih installed it, so a rebuilt stream box came up without it. The kiosk-panel step (16h)
now installs the pinned official .deb, sha256-verified, on the stream role only (the strih boxes are
production-only), and the panel grader (verify item 44) carries a `(panel-discord)` row.
"""
import hashlib
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
DEB = b"fake discord deb bytes\n"


def _bash(body, *args, env=None, box="stream-pp"):
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\nshift 3\n' + body
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "h", str(FACTS), box, str(PROVISION), *args],
                          capture_output=True, text=True, env=e, timeout=120)


def _exec(path, text):
    path.write_text(text)
    path.chmod(0o755)


class Box:
    def __init__(self, tmp, installed=False, sha=None):
        self.bin = tmp / "bin"
        self.bin.mkdir()
        self.log = tmp / "calls"
        self.state = tmp / "state"
        self.state.mkdir()
        if installed:
            (self.state / "discord").write_text("")
        _exec(self.bin / "curl",
              '#!/bin/bash\necho "curl $*" >> %s\nout=""\nwhile [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done\n'
              'printf %%s %s > "$out"\n' % (self.log, "'" + DEB.decode().rstrip("\n") + "\n'"))
        _exec(self.bin / "apt-get",
              '#!/bin/bash\necho "apt-get $*" >> %s\ntouch %s/discord\n' % (self.log, self.state))
        _exec(self.bin / "dpkg-query",
              '#!/bin/bash\nfor a in "$@"; do last="$a"; done\n'
              '[ "$last" = discord ] && [ -e %s/discord ] && printf "install ok installed"\nexit 0\n' % self.state)
        self.sha = sha if sha is not None else hashlib.sha256(DEB).hexdigest()

    def env(self):
        return {"PATH": "%s:%s" % (self.bin, os.environ["PATH"]),
                "OBS_BOX_PANEL_DISCORD_SHA256": self.sha,
                "OBS_BOX_PANEL_DISCORD_URL": "https://example.invalid/discord-test.deb"}

    def calls(self):
        return self.log.read_text() if self.log.exists() else ""


def test_the_stream_role_installs_the_pinned_deb_after_its_sha256_matched(tmp_path):
    box = Box(tmp_path)
    r = _bash("obs_box_panel_discord_install", env=box.env())
    assert r.returncode == 0, r.stderr
    calls = box.calls()
    assert "curl -fsSL -o " in calls and "https://example.invalid/discord-test.deb" in calls, calls
    assert "apt-get install -y " in calls and ".deb" in calls.split("apt-get install -y ", 1)[1], calls


def test_a_sha256_mismatch_installs_nothing_and_fails_naming_the_url(tmp_path):
    box = Box(tmp_path, sha="0" * 64)
    r = _bash("obs_box_panel_discord_install", env=box.env())
    assert r.returncode != 0
    assert "sha256" in r.stderr and "https://example.invalid/discord-test.deb" in r.stderr, r.stderr
    assert "apt-get" not in box.calls()


def test_an_installed_discord_is_not_downloaded_again(tmp_path):
    box = Box(tmp_path, installed=True)
    r = _bash("obs_box_panel_discord_install", env=box.env())
    assert r.returncode == 0, r.stderr
    assert box.calls() == ""


def test_the_strih_role_never_installs_discord(tmp_path):
    box = Box(tmp_path)
    r = _bash("obs_box_panel_discord_install", env=box.env(), box="strih-pp")
    assert r.returncode == 0, r.stderr
    assert box.calls() == ""


def test_the_panel_step_runs_the_discord_install_before_the_launchers():
    text = (SCRIPTS / "lib" / "obs-box-panel.sh").read_text()
    body = text[text.index("obs_box_panel_install() {"):]
    body = body[:body.index("\n}\n")]
    assert "obs_box_panel_discord_install" in body
    assert body.index("obs_box_panel_discord_install") < body.index("done < <(obs_box_panel_launchers")


def test_the_grade_row_is_ok_installed_fail_missing_on_the_stream_role(tmp_path):
    (tmp_path / "a").mkdir()
    (tmp_path / "b").mkdir()
    ok = _bash("obs_box_panel_discord_row", env=Box(tmp_path / "a", installed=True).env())
    assert ok.stdout.startswith("OK|(panel-discord) "), ok.stdout + ok.stderr
    bad = _bash("obs_box_panel_discord_row", env=Box(tmp_path / "b").env())
    assert bad.stdout.startswith("FAIL|(panel-discord) ") and "16h" in bad.stdout, bad.stdout + bad.stderr


def test_the_grade_row_is_a_note_on_the_strih_role(tmp_path):
    r = _bash("obs_box_panel_discord_row", env=Box(tmp_path).env(), box="strih-pp")
    assert r.stdout.startswith("NOTE|(panel-discord) "), r.stdout + r.stderr


def test_the_panel_grader_now_prints_ten_rows():
    # six since issue 1448 (+ panel-discord), seven since issue 1441 (+ panel-kbd), eight since issue 1458
    # (+ panel-files), nine since issue 1464 (+ panel-power), ten since issue 1481 (+ panel-shot)
    assert _bash("printf %s $OBS_BOX_PANEL_ROWS").stdout == "10"
