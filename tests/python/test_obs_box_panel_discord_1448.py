"""issue 1448 -- Discord as a launcher on the left of the kiosk panel (issue 1420's tint2 panel).

Owner 16.10.2026 (verbatim): "ma byt na lavo discord v liste". Discord was hand-installed on stream-pp
and, after it crashed, no launcher could start it again.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"


def _bash(body, *args, env=None, box="stream-pp"):
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\nshift 3\n' + body
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "h", str(FACTS), box, str(PROVISION), *args],
                          capture_output=True, text=True, env=e, timeout=120)


def _home(tmp_path, discord=True):
    home = tmp_path / "home"
    ud = home / ".config" / "systemd" / "user"
    ud.mkdir(parents=True)
    (ud / "strih-obs.service").write_text("[Service]\n")
    sysd = tmp_path / "sysunits"
    sysd.mkdir()
    dc = tmp_path / "discord"
    if discord:
        dc.write_text("#!/bin/sh\n")
        dc.chmod(0o755)
    env = {"OBS_BOX_PANEL_SYS_UNIT_DIR": str(sysd),
           "OBS_BOX_PANEL_RUSTDESK_BIN": str(tmp_path / "no-rustdesk"),
           "OBS_BOX_PANEL_DISCORD_BIN": str(dc),
           "OBS_BOX_PANEL_CHROME_BIN": str(tmp_path / "no-chrome")}   # issue 1464: no host Chrome leaks in
    return home, env


def test_a_box_with_discord_gets_a_discord_launcher_that_starts_the_binary(tmp_path):
    home, env = _home(tmp_path)
    r = _bash('obs_box_panel_launchers "$1"', str(home), env=env)
    rows = [ln.split("|") for ln in r.stdout.splitlines()]
    assert [x[0] for x in rows] == ["power", "obs", "discord"], r.stdout + r.stderr   # issue 1464: power first
    discord = rows[2]
    assert discord[1] == "Discord"
    assert discord[2] == str(tmp_path / "discord")   # single-instance: a click focuses a running one
    assert discord[3] == "discord"                   # the icon the Discord .deb installs


def test_a_box_without_discord_gets_no_discord_launcher(tmp_path):
    home, env = _home(tmp_path, discord=False)
    r = _bash('obs_box_panel_launchers "$1"', str(home), env=env)
    assert [ln.split("|")[0] for ln in r.stdout.splitlines()] == ["power", "obs"], r.stdout + r.stderr


def test_the_config_lists_discord_on_the_left_with_the_other_launchers(tmp_path):
    home, env = _home(tmp_path)
    r = _bash('obs_box_panel_tint2rc "$1"', str(home), env=env)
    cfg = [ln for ln in r.stdout.splitlines() if not ln.startswith("#")]
    assert "panel_items = LTSEC" in cfg              # launchers (L) left of the taskbar (T); E = layout, issue 1441
    apps = [ln.split(" = ", 1)[1] for ln in cfg if ln.startswith("launcher_item_app = ")]
    assert apps == [str(home / (".local/share/applications/obs-box-panel-%s.desktop" % i))
                    for i in ("power", "obs", "discord")]
