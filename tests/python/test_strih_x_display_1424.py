"""Issue 1424 (owner 24.10.2026: "to musi vediet fungovat aj bez monitora aj po zapojeni monitora aj po
zapnuti!!!"; main design 6386467303, layer 1) -- X never depends on a monitor and never holds a fixed
HDMI output.

strih-pp's X put the Multiview monitor (HDMI-0, a fixed vk-direct output) into its layout at the
24.10.2026 dark boot, and OBS's acquire of it read -13. `scripts/lib/strih-x-display.sh` writes ONE
xorg.conf.d OutputClass for the NVIDIA driver: MetaModes over the OPERATOR outputs only (every NVIDIA
connector X lists minus the fixed ones of the drm-output config) and AllowEmptyInitialConfiguration.
setup-strih step 6 writes it (write-if-changed, removed when nothing is armed, never an X restart);
verify-strih's (display-x-config) row grades it, the running X's log and the fixed outputs' X state.

Tier-0: the real lib under the callers' set -euo pipefail, a fake xrandr on PATH, the live strih-pp /
strih-snv xrandr texts of 24.10.2026, seams for the config dir and the Xorg log.
"""
import json
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
XLIB = SCRIPTS / "lib" / "strih-x-display.sh"
DRM_LIB = SCRIPTS / "lib" / "strih-drm-output.sh"
APPS_LIB = SCRIPTS / "lib" / "strih-session-apps.sh"

PP = """Screen 0: minimum 8 x 8, current 8 x 8, maximum 32767 x 32767
HDMI-0 connected primary (normal left inverted right x axis y axis)
   2560x1440     59.95 +
DP-0 disconnected (normal left inverted right x axis y axis)
DP-1 disconnected (normal left inverted right x axis y axis)
HDMI-1 disconnected (normal left inverted right x axis y axis)
DP-2 disconnected (normal left inverted right x axis y axis)
DP-3 disconnected (normal left inverted right x axis y axis)
"""
PP_X_HOLDS_HDMI0 = PP.replace("HDMI-0 connected primary (normal", "HDMI-0 connected primary 2560x1440+0+0 (normal")
SNV = """Screen 0: minimum 8 x 8, current 2560 x 1600, maximum 32767 x 32767
HDMI-0 disconnected (normal left inverted right x axis y axis)
DP-0 disconnected (normal left inverted right x axis y axis)
eDP-1-2 connected primary 2560x1600+0+0 (normal left inverted right x axis y axis) 329mm x 206mm
DP-1-1 disconnected (normal left inverted right x axis y axis)
DP-1-5 disconnected (normal left inverted right x axis y axis)
"""
PP_MM = ("DP-0: nvidia-auto-select, DP-1: nvidia-auto-select, DP-2: nvidia-auto-select, "
         "DP-3: nvidia-auto-select")
PP_CONFIG = {"enabled": True, "outputs": [
    {"connector": "HDMI-0", "argb": 2105376, "view": "multiview", "backend": "vk-direct"},
    {"connector": "HDMI-1", "argb": 2105376, "view": "program", "backend": "vk-direct", "audio": False}]}


def _run(body, tmp_path, xrandr=None, env=None, sources=(XLIB,)):
    b = tmp_path / "bin"
    b.mkdir(exist_ok=True)
    if xrandr is not None:
        (tmp_path / "xrandr.out").write_text(xrandr)
        p = b / "xrandr"
        p.write_text('#!/bin/bash\ncat "%s"\n' % (tmp_path / "xrandr.out"))
        p.chmod(0o755)
    idp = b / "id"
    idp.write_text("#!/bin/bash\necho 1000\n")
    idp.chmod(0o755)
    harness = ("set -euo pipefail\nwarn() { echo \"WARN $*\"; }\n"
               + "".join('. "%s"\n' % s for s in sources) + body)
    e = dict(os.environ, PATH="%s:%s" % (b, os.environ["PATH"]),
             STRIH_X_CONF_DIR=str(tmp_path / "xorg.conf.d"), STRIH_X_XORG_LOG=str(tmp_path / "Xorg.0.log"),
             STRIH_SESSION_APPS_EUID="1000")
    if env:
        e.update(env)
    return subprocess.run(["bash", "-c", harness], capture_output=True, text=True, env=e, cwd=str(REPO),
                          timeout=60)


def _home(tmp_path, config=PP_CONFIG):
    home = tmp_path / "home"
    (home / ".camera-box").mkdir(parents=True, exist_ok=True)
    if config is not None:
        (home / ".camera-box" / "drm-output.json").write_text(json.dumps(config, separators=(",", ":")) + "\n")
    return home


# --- the pure renderers --------------------------------------------------------------------------------

def test_nvidia_outputs_skip_the_igpu_sink_outputs(tmp_path):
    r = _run("strih_x_display_nvidia_outputs", tmp_path)
    assert r.returncode == 0
    r = subprocess.run(["bash", "-c", '. "%s"; strih_x_display_nvidia_outputs' % XLIB], input=SNV,
                       capture_output=True, text=True)
    assert r.stdout.split() == ["HDMI-0", "DP-0"]
    r = subprocess.run(["bash", "-c", '. "%s"; strih_x_display_nvidia_outputs' % XLIB], input=PP,
                       capture_output=True, text=True)
    assert r.stdout.split() == ["HDMI-0", "DP-0", "DP-1", "HDMI-1", "DP-2", "DP-3"]


def test_the_conf_names_only_the_operator_outputs(tmp_path):
    r = _run('strih_x_display_conf_text "HDMI-0 HDMI-1" "HDMI-0 DP-0 DP-1 HDMI-1 DP-2 DP-3"', tmp_path)
    assert r.returncode == 0, r.stderr
    t = r.stdout
    assert 'MatchDriver "nvidia-drm"' in t
    assert 'Option "AllowEmptyInitialConfiguration" "true"' in t
    assert 'Option "MetaModes" "%s"' % PP_MM in t
    mm = t.split('Option "MetaModes"', 1)[1]
    assert "HDMI" not in mm, "a fixed output is never named in the MetaModes"
    assert t.count('Section "OutputClass"') == 1 and t.rstrip().endswith("EndSection")


def test_strih_snv_keeps_hdmi0_out_with_dp0_as_its_operator_output(tmp_path):
    r = _run('strih_x_display_conf_text "HDMI-0" "HDMI-0 DP-0"', tmp_path)
    assert 'Option "MetaModes" "DP-0: nvidia-auto-select"' in r.stdout


def test_the_conf_refuses_what_it_cannot_write_safely(tmp_path):
    for fixed, outs in (("", "HDMI-0 DP-0"), ("HDMI-0", "HDMI-0"), ('HDMI-0"', "HDMI-0 DP-0"),
                        ("HDMI-0", 'DP-0" Option')):
        r = _run('strih_x_display_conf_text \'%s\' \'%s\' || echo REFUSED' % (fixed, outs), tmp_path)
        assert r.stdout.strip() == "REFUSED", (fixed, outs, r.stdout)


def test_fixed_reads_the_config_like_the_c_module(tmp_path):
    home = _home(tmp_path)
    conf = home / ".camera-box" / "drm-output.json"
    r = _run('strih_x_display_fixed "%s" "%s"' % (SCRIPTS, conf), tmp_path)
    assert r.returncode == 0 and r.stdout.strip() == "HDMI-0 HDMI-1", r.stderr
    r = _run('strih_x_display_fixed "%s" "%s"' % (SCRIPTS, tmp_path / "absent.json"), tmp_path)
    assert r.returncode == 0 and r.stdout.strip() == ""
    conf.write_text('{"enabled":true,"outputs":[]}\n')
    r = _run('strih_x_display_fixed "%s" "%s" || echo CANNOT' % (SCRIPTS, conf), tmp_path)
    assert r.stdout.strip() == "CANNOT"


# --- setup-strih step 6 ----------------------------------------------------------------------------------

def _provision(tmp_path, home, xrandr=PP):
    return _run('strih_x_display_provision "%s" newlevel "%s" "%s"'
                % (home, SCRIPTS, home / ".camera-box" / "drm-output.json"),
                tmp_path, xrandr=xrandr, sources=(DRM_LIB,))


def test_step_6_writes_the_conf_once_and_never_restarts_x(tmp_path):
    home = _home(tmp_path)
    r = _provision(tmp_path, home)
    assert r.returncode == 0, r.stderr
    conf = tmp_path / "xorg.conf.d" / "15-camera-box-fixed-hdmi.conf"
    assert 'Option "MetaModes" "%s"' % PP_MM in conf.read_text()
    assert "wrote" in r.stdout and "next X start" in r.stdout
    mtime = conf.stat().st_mtime_ns
    r = _provision(tmp_path, home)
    assert "unchanged" in r.stdout and conf.stat().st_mtime_ns == mtime
    assert "systemctl" not in r.stdout and "lightdm" not in r.stdout


def test_step_6_removes_the_conf_when_nothing_is_armed(tmp_path):
    home = _home(tmp_path)
    _provision(tmp_path, home)
    (home / ".camera-box" / "drm-output.json").unlink()
    r = _provision(tmp_path, home)
    assert r.returncode == 0 and "removed" in r.stdout
    assert not (tmp_path / "xorg.conf.d" / "15-camera-box-fixed-hdmi.conf").exists()


def test_step_6_skips_loudly_when_x_does_not_answer(tmp_path):
    home = _home(tmp_path)
    r = _provision(tmp_path, home, xrandr="")
    assert r.returncode == 0 and "WARN" in r.stdout and "NOT written" in r.stdout
    assert not (tmp_path / "xorg.conf.d").exists()


def test_step_6_runs_it_after_the_drm_output_config():
    body = DRM_LIB.read_text().split("strih_drm_output_provision() {", 1)[1]
    assert body.index('strih_x_display_provision "$user_home" "$desktop_user" "$scripts_dir" "$DRM_CONF"') \
        < body.index("drm_output_audio_wp_provision")


# --- verify-strih's (display-x-config) row -----------------------------------------------------------------

def _grade(tmp_path, home, xrandr=PP, log=""):
    (tmp_path / "Xorg.0.log").write_text(log)
    r = _run('strih_x_display_grade_row "%s" "%s" newlevel' % (REPO, home), tmp_path, xrandr=xrandr,
             sources=(APPS_LIB,))
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_the_row_is_ok_when_the_running_x_applied_it(tmp_path):
    home = _home(tmp_path)
    _provision(tmp_path, home)
    row = _grade(tmp_path, home, log='[    10.546] (**) NVIDIA(0): Option "MetaModes" "%s"\n' % PP_MM)
    assert row.startswith("OK|(display-x-config) ") and "live in the running X" in row, row


def test_the_row_notes_a_conf_the_running_x_has_not_read_yet(tmp_path):
    home = _home(tmp_path)
    _provision(tmp_path, home)
    row = _grade(tmp_path, home, log='[    10.546] (**) NVIDIA(0): Option "AllowEmptyInitialConfiguration"\n')
    assert row.startswith("NOTE|(display-x-config) ") and "next X start" in row, row


def test_the_row_fails_a_missing_or_stale_conf_and_a_fixed_output_in_x(tmp_path):
    home = _home(tmp_path)
    row = _grade(tmp_path, home)
    assert row.startswith("FAIL|") and "missing" in row, row
    _provision(tmp_path, home)
    conf = tmp_path / "xorg.conf.d" / "15-camera-box-fixed-hdmi.conf"
    conf.write_text(conf.read_text().replace("DP-3: nvidia-auto-select", "HDMI-1: nvidia-auto-select"))
    assert _grade(tmp_path, home).startswith("FAIL|"), "a hand-edited conf"
    _provision(tmp_path, home)
    row = _grade(tmp_path, home, xrandr=PP_X_HOLDS_HDMI0)
    assert row.startswith("FAIL|") and "HDMI-0" in row and "X layout" in row, row


def test_the_row_is_ok_on_a_box_with_no_fixed_output(tmp_path):
    home = _home(tmp_path, config=None)
    row = _grade(tmp_path, home)
    assert row == "OK|(display-x-config) no fixed HDMI output armed -- no X config needed"


# --- the role's HDMI policy (independent review) --------------------------------------------------------

def test_an_unarmed_hdmi_is_never_an_operator_output_in_the_x_config(tmp_path):
    # step 6 runs on the strih role only, where no HDMI is ever the desktop (issue 1346)
    r = _run('strih_x_display_metamodes "HDMI-0" "DP-0 HDMI-0 HDMI-2 DP-1"', tmp_path)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "DP-0: nvidia-auto-select, DP-1: nvidia-auto-select"


def _policy(tmp_path, role_has_hdmi, envfile=None):
    tmp_path.mkdir(parents=True, exist_ok=True)
    home = _home(tmp_path, config=None)
    if envfile is not None:
        (home / ".config").mkdir(parents=True, exist_ok=True)
        (home / ".config" / "strih-display-watch.env").write_text(envfile)
    gate = "obs_box_step_applies() { return %d; }\n" % (0 if role_has_hdmi else 1)
    return home, gate


def test_the_policy_file_follows_the_role(tmp_path):
    home, gate = _policy(tmp_path, True)
    r = _run(gate + 'strih_display_watch_env_text; echo; strih_display_watch_env_put "%s"' % home, tmp_path)
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines()[0] == "STRIH_DISPLAY_WATCH_HDMI=off"
    assert "written" in r.stdout
    assert (home / ".config" / "strih-display-watch.env").read_text() == "STRIH_DISPLAY_WATCH_HDMI=off\n"
    r = _run(gate + 'strih_display_watch_env_put "%s"' % home, tmp_path)
    assert r.stdout.strip() == "unchanged"
    _home2, gate2 = _policy(tmp_path / "s", False)
    r = _run(gate2 + 'strih_display_watch_env_text', tmp_path)
    assert r.stdout == "STRIH_DISPLAY_WATCH_HDMI=operator"


def test_the_row_fails_a_policy_file_the_role_does_not_want(tmp_path):
    home, gate = _policy(tmp_path, True, envfile="STRIH_DISPLAY_WATCH_HDMI=operator\n")
    r = _run(gate + 'strih_x_display_grade_row "%s" "%s" "$(whoami)"' % (REPO, home), tmp_path, xrandr=SNV,
             sources=(XLIB, APPS_LIB))
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("FAIL|(display-x-config)") and "strih-display-watch.env" in r.stdout, r.stdout
    home, gate = _policy(tmp_path / "ok", True, envfile="STRIH_DISPLAY_WATCH_HDMI=off\n")
    r = _run(gate + 'strih_x_display_grade_row "%s" "%s" "$(whoami)"' % (REPO, home), tmp_path, xrandr=SNV,
             sources=(XLIB, APPS_LIB))
    assert r.stdout.startswith("OK|(display-x-config)"), r.stdout
