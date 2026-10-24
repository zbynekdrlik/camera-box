"""Issue 1424 (owner 24.10.2026: "to musi vediet fungovat aj bez monitora aj po zapojeni monitora aj po
zapnuti!!!"; main design 6386467303, layer 2) -- the session display watch.

strih-pp booted on 24.10.2026 with the venue dark: X ran an 8x8 screen with no monitor, tint2 (the kiosk
panel) segfaulted in a restart loop on that empty screen, and when the operator's NEC came on nothing put
it into the X layout (openbox kiosk, no desktop daemon). `scripts/strih_display_watch.py` is a session
app (`strih-display-watch.service`) that reads `xrandr --current` every 2 s and runs the pure `decide`:

  * every connected OPERATOR output (any output that is not a fixed HDMI output of the drm-output
    config) with no mode gets `--auto`, right of the operator outputs already on, the first one primary;
  * a fixed (vk-direct / lease) output that X put into its layout goes `--off` -- never one OBS holds
    (X then shows it with no mode, or disconnected), never an output it cannot classify;
  * with no operator output on, the screen is kept at least 1920x1080 (`--fb`) and the primary flag is
    cleared, so tint2 and OBS always have a screen;
  * an operator output that is on is never touched, unplugged or not.

The panel waits for a monitor through the same program (`--exec-when-screen`), so tint2 never starts on
an empty screen. Tier-0 pytest, the strih-pp / strih-snv xrandr texts read live 24.10.2026 as fixtures.
"""
import importlib.util
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
WATCH = SCRIPTS / "strih_display_watch.py"
UNIT = REPO / "systemd" / "strih-display-watch.service"
PANEL_UNIT = REPO / "systemd" / "obs-box-panel.service"
SESSION_LIB = SCRIPTS / "lib" / "strih-session-apps.sh"
ROLE_LIB = SCRIPTS / "lib" / "obs-box-role.sh"


def _load():
    sys.path.insert(0, str(SCRIPTS))
    spec = importlib.util.spec_from_file_location("strih_display_watch", WATCH)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


W = _load()

# strih-pp, 24.10.2026 18:12, venue dark after the 17:52 boot (read live)
PP_DARK = """Screen 0: minimum 8 x 8, current 8 x 8, maximum 32767 x 32767
HDMI-0 connected primary (normal left inverted right x axis y axis)
   2560x1440     59.95 +
   1920x1080     60.00    59.94    50.00
DP-0 disconnected (normal left inverted right x axis y axis)
DP-1 disconnected (normal left inverted right x axis y axis)
HDMI-1 disconnected (normal left inverted right x axis y axis)
DP-2 disconnected (normal left inverted right x axis y axis)
DP-3 disconnected (normal left inverted right x axis y axis)
"""

# the same box right after X started with only the BenQ: X drives HDMI-0 (the boot MetaMode)
PP_DARK_X_HOLDS_HDMI0 = """Screen 0: minimum 8 x 8, current 2560 x 1440, maximum 32767 x 32767
HDMI-0 connected primary 2560x1440+0+0 (normal left inverted right x axis y axis) 597mm x 336mm
   2560x1440     59.95*+
DP-0 disconnected (normal left inverted right x axis y axis)
DP-1 disconnected (normal left inverted right x axis y axis)
HDMI-1 disconnected (normal left inverted right x axis y axis)
DP-2 disconnected (normal left inverted right x axis y axis)
DP-3 disconnected (normal left inverted right x axis y axis)
"""

# the NEC comes on (DP-2), X has not put it into the layout
PP_NEC_ON = """Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767
HDMI-0 connected primary (normal left inverted right x axis y axis)
   2560x1440     59.95 +
DP-0 disconnected (normal left inverted right x axis y axis)
DP-1 disconnected (normal left inverted right x axis y axis)
HDMI-1 disconnected (normal left inverted right x axis y axis)
DP-2 connected (normal left inverted right x axis y axis)
   1920x1200     59.95 +
DP-3 disconnected (normal left inverted right x axis y axis)
"""

# steady production: the NEC on and primary, OBS holds HDMI-0 + HDMI-1 (X shows them with no mode)
PP_STEADY = """Screen 0: minimum 8 x 8, current 1920 x 1200, maximum 32767 x 32767
HDMI-0 connected (normal left inverted right x axis y axis)
DP-0 disconnected (normal left inverted right x axis y axis)
DP-1 disconnected (normal left inverted right x axis y axis)
HDMI-1 disconnected (normal left inverted right x axis y axis)
DP-2 connected primary 1920x1200+0+0 (normal left inverted right x axis y axis) 495mm x 310mm
   1920x1200     59.95*+
DP-3 disconnected (normal left inverted right x axis y axis)
"""

# strih-snv, 24.10.2026 (read live): the notebook panel on the iGPU sink, HDMI-0 empty
SNV = """Screen 0: minimum 8 x 8, current 2560 x 1600, maximum 32767 x 32767
HDMI-0 disconnected (normal left inverted right x axis y axis)
DP-0 disconnected (normal left inverted right x axis y axis)
eDP-1-2 connected primary 2560x1600+0+0 (normal left inverted right x axis y axis) 329mm x 206mm
   2560x1600    240.00*+  60.00
DP-1-1 disconnected (normal left inverted right x axis y axis)
DP-1-2 disconnected (normal left inverted right x axis y axis)
"""

PP_FIXED = ["HDMI-0", "HDMI-1"]
PP_CONFIG = ('{"enabled":true,"outputs":[{"connector":"HDMI-0","argb":2105376,"view":"multiview",'
             '"backend":"vk-direct"},{"connector":"HDMI-1","argb":2105376,"view":"program",'
             '"backend":"vk-direct","audio":false}]}')


def _decide(text, fixed):
    screen, outs = W.parse_xrandr(text)
    return W.decide(screen, outs, fixed)


# --- parsing ---------------------------------------------------------------------------------------

def test_parse_reads_the_live_strih_pp_dark_screen():
    screen, outs = W.parse_xrandr(PP_DARK)
    assert screen == (8, 8)
    by = {o.name: o for o in outs}
    assert [o.name for o in outs] == ["HDMI-0", "DP-0", "DP-1", "HDMI-1", "DP-2", "DP-3"]
    assert by["HDMI-0"].connected and by["HDMI-0"].primary and not by["HDMI-0"].active
    assert not by["DP-2"].connected and not by["DP-2"].active


def test_parse_reads_geometry_of_an_active_output():
    _screen, outs = W.parse_xrandr(SNV)
    edp = {o.name: o for o in outs}["eDP-1-2"]
    assert edp.active and edp.primary and (edp.w, edp.h, edp.x, edp.y) == (2560, 1600, 0, 0)


def test_parse_of_garbage_is_no_screen():
    assert W.parse_xrandr("") == (None, [])
    assert W.parse_xrandr("Can't open display :0\n") == (None, [])


# --- the decision ----------------------------------------------------------------------------------

def test_dark_boot_keeps_a_screen_and_never_puts_a_fixed_output_into_x():
    args = _decide(PP_DARK, PP_FIXED)
    assert args == ["--fb", "1920x1080", "--noprimary"]
    assert "HDMI-0" not in args and "--auto" not in args


def test_a_fixed_output_x_drives_goes_off_with_the_screen_kept():
    args = _decide(PP_DARK_X_HOLDS_HDMI0, PP_FIXED)
    # --fb first: turning the only output off would shrink the screen to 8x8 (the 24.10 state)
    assert args == ["--fb", "2560x1440", "--noprimary", "--output", "HDMI-0", "--off"]


def test_an_operator_monitor_that_comes_on_is_put_into_the_layout_as_primary():
    assert _decide(PP_NEC_ON, PP_FIXED) == ["--output", "DP-2", "--auto", "--primary"]


def test_steady_production_and_strih_snv_are_left_alone():
    assert _decide(PP_STEADY, PP_FIXED) == []
    assert _decide(SNV, ["HDMI-0"]) == []


def test_a_second_operator_monitor_goes_right_of_the_first():
    text = PP_STEADY.replace("DP-0 disconnected (normal", "DP-0 connected (normal")
    assert _decide(text, PP_FIXED) == ["--output", "DP-0", "--auto", "--right-of", "DP-2"]


def test_an_active_operator_output_without_primary_gets_it():
    text = PP_STEADY.replace("DP-2 connected primary 1920", "DP-2 connected 1920")
    assert _decide(text, PP_FIXED) == ["--output", "DP-2", "--primary"]


def test_an_unplugged_operator_output_that_is_still_on_is_never_touched():
    text = PP_STEADY.replace("DP-2 connected primary", "DP-2 disconnected primary")
    assert _decide(text, PP_FIXED) == []


def test_an_unclassifiable_config_touches_no_output_only_the_screen():
    assert _decide(PP_DARK, None) == ["--fb", "1920x1080"]
    assert _decide(PP_NEC_ON, None) == []


def test_without_a_fixed_output_every_connected_output_is_an_operator_output():
    # a stream box (no drm-output config): an HDMI monitor plugged in joins the desktop
    assert _decide(PP_DARK, []) == ["--output", "HDMI-0", "--auto", "--primary"]


def test_an_output_obs_holds_is_never_named():
    for text in (PP_DARK, PP_NEC_ON, PP_STEADY):
        args = _decide(text, PP_FIXED)
        assert "HDMI-1" not in args
        assert "HDMI-0" not in args


def test_screen_ready_needs_a_monitor_and_a_real_screen():
    for text, want in ((PP_DARK, False), (PP_STEADY, True), (SNV, True), ("", False)):
        screen, outs = W.parse_xrandr(text)
        assert W.screen_ready(screen, outs) is want, text[:40]


# --- the drm-output config ---------------------------------------------------------------------------

def test_fixed_connectors_read_the_config_like_the_c_module():
    assert W.fixed_connectors("") == []
    assert W.fixed_connectors(PP_CONFIG) == PP_FIXED
    assert W.fixed_connectors('{"enabled":false,"connector":"HDMI-0"}') == []
    assert W.fixed_connectors('{"enabled":true,"outputs":[]}') is None
    assert W.fixed_connectors("not json") is None


# --- the loop ----------------------------------------------------------------------------------------

class Rig:
    """A fake X: xrandr --current prints `text`; an apply records its argv and answers `rc`."""

    def __init__(self, text, rc=0):
        self.text, self.rc, self.applied, self.reads = text, rc, [], 0

    def run(self, argv, timeout):
        assert timeout > 0
        if argv[1:] == ["--current"]:
            self.reads += 1
            return 0, self.text, ""
        self.applied.append(argv[1:])
        return self.rc, "", "xrandr: Configure crtc 0 failed" if self.rc else ""


def _watch(tmp_path, rig, config=PP_CONFIG):
    logs = []
    w = W.Watch(state_file=str(tmp_path / "w.json"), run=rig.run, log=logs.append,
                config_text=lambda: config)
    return w, logs


def test_a_pass_applies_once_and_logs_once(tmp_path):
    rig = Rig(PP_NEC_ON)
    w, logs = _watch(tmp_path, rig)
    w.pass_once(now=100.0)
    assert rig.applied == [["--output", "DP-2", "--auto", "--primary"]]
    rig.text = PP_STEADY
    w.pass_once(now=102.0)
    w.pass_once(now=104.0)
    assert len(rig.applied) == 1
    assert sum("xrandr --output DP-2 --auto --primary" in ln for ln in logs) == 1, logs
    st = json.loads((tmp_path / "w.json").read_text())
    assert st["screen"] == [1920, 1200] and st["fixed"] == PP_FIXED and st["last_error"] is None


def test_a_failed_apply_is_retried_after_a_pause_and_logged_once(tmp_path):
    rig = Rig(PP_NEC_ON, rc=1)
    w, logs = _watch(tmp_path, rig)
    w.pass_once(now=100.0)
    w.pass_once(now=102.0)
    assert len(rig.applied) == 1, "a failed apply waits RETRY_FAILED_S before the same change again"
    w.pass_once(now=100.0 + W.RETRY_FAILED_S)
    assert len(rig.applied) == 2
    assert sum("failed" in ln for ln in logs) == 1, logs
    assert json.loads((tmp_path / "w.json").read_text())["last_error"]


def test_x_not_answering_is_logged_once_and_nothing_applied(tmp_path):
    rig = Rig("")
    w, logs = _watch(tmp_path, rig)
    for t in range(5):
        w.pass_once(now=100.0 + 2 * t)
    assert rig.applied == []
    assert sum("not answering" in ln for ln in logs) == 1, logs


# --- --check-state (verify-strih's pass row) ------------------------------------------------------------

def _state(tmp_path, **over):
    st = {"version": W.STATE_VERSION, "updated_epoch_s": time.time() - 2, "screen": [1920, 1200],
          "outputs": [["DP-2", "connected", "on", "primary"]], "fixed": PP_FIXED, "last_apply": None,
          "last_error": None, "pending": []}
    st.update(over)
    p = tmp_path / "s.json"
    p.write_text(json.dumps(st))
    return p


def test_check_state_ok_names_the_screen_and_the_outputs(tmp_path):
    ok, line = W.check_state_file(str(_state(tmp_path)))
    assert ok, line
    assert "1920x1200" in line and "DP-2" in line and "HDMI-0" in line


def test_check_state_fails_stale_a_tiny_screen_and_a_failing_apply(tmp_path):
    assert not W.check_state_file(str(_state(tmp_path, updated_epoch_s=time.time() - 60)))[0]
    assert not W.check_state_file(str(_state(tmp_path, screen=[8, 8], outputs=[])))[0]
    assert not W.check_state_file(str(_state(tmp_path, last_error="Configure crtc 0 failed",
                                              pending=["--output", "DP-2", "--auto"])))[0]
    assert not W.check_state_file(str(tmp_path / "absent.json"))[0]


# --- --exec-when-screen (the panel's guard) ------------------------------------------------------------

def test_exec_when_screen_waits_for_a_monitor_then_execs(tmp_path):
    texts = [PP_DARK, PP_DARK, PP_STEADY]
    rig = Rig(texts[0])
    calls = []

    def run(argv, timeout):
        rig.text = texts[min(rig.reads, len(texts) - 1)]
        return rig.run(argv, timeout)

    logs = []
    W.exec_when_screen(["/usr/bin/tint2", "-c", "rc"], run=run, sleep=lambda s: None,
                       execv=lambda p, a: calls.append((p, a)), log=logs.append)
    assert calls == [("/usr/bin/tint2", ["/usr/bin/tint2", "-c", "rc"])]
    assert sum("waiting" in ln for ln in logs) == 1, logs


# --- the unit, the session-app wiring, the panel guard -------------------------------------------------

def _unit():
    return {ln.split("=", 1)[0]: ln.split("=", 1)[1]
            for ln in UNIT.read_text().splitlines() if "=" in ln and not ln.startswith("#")}


def test_the_unit_is_a_supervised_session_app():
    u = _unit()
    assert u["ExecStart"] == ("/usr/bin/python3 /usr/local/bin/strih_display_watch.py "
                              "--state-file %t/strih-display-watch.json")
    assert u["Environment"] == "DISPLAY=:0"
    assert u["Restart"] == "always" and u["StartLimitIntervalSec"] == "0"
    assert u["WantedBy"] == "graphical-session.target"
    assert "graphical-session.target" in u["After"].split()


def _bash(body):
    r = subprocess.run(["bash", "-c", "set -euo pipefail\n. \"%s\"\n%s" % (SESSION_LIB, body)],
                       capture_output=True, text=True, cwd=str(REPO), timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_both_roles_run_the_watch_through_the_session_app_framework():
    out = _bash('printf "%s|%s|%s\\n" "${STRIH_SESSION_APP_UNITS[*]}" '
                '"$(strih_session_app_script strih-display-watch.service)" '
                '"$(strih_session_app_item strih-display-watch.service)"')
    units, script, item = out.strip().split("|")
    assert "strih-display-watch.service" in units.split()
    assert (script, item) == ("strih_display_watch.py", "display-watch")
    assert "strih-display-watch.service" in ROLE_LIB.read_text().split(
        "OBS_BOX_STREAM_SESSION_APP_UNITS=(", 1)[1].split(")", 1)[0]


def test_the_panel_starts_tint2_only_once_x_has_a_screen():
    u = {ln.split("=", 1)[0]: ln.split("=", 1)[1]
         for ln in PANEL_UNIT.read_text().splitlines() if "=" in ln and not ln.startswith("#")}
    assert u["ExecStart"] == ("/usr/bin/python3 /usr/local/bin/strih_display_watch.py --exec-when-screen "
                              "/usr/bin/tint2 -c %h/.config/tint2/tint2rc")


# --- the role's HDMI policy (independent review: the strih role never puts HDMI on the desktop) -----

def test_on_the_strih_role_an_unarmed_hdmi_output_is_kept_out_like_a_fixed_one():
    # the issue-1346 strih-role contract (the kiosk autostart turns every ^HDMI off): with the drm-output
    # config absent or rolled back, the watch must not --auto HDMI back onto the desktop
    screen, outs = W.parse_xrandr(PP_DARK)
    eff = W.effective_fixed([], outs, hdmi_off=True)
    assert sorted(eff) == ["HDMI-0", "HDMI-1"]
    # the same args as the armed dark boot (test_dark_boot_keeps_a_screen_and_never_puts_a_fixed_output_into_x)
    assert W.decide(screen, outs, eff) == ["--fb", "1920x1080", "--noprimary"]
    # an HDMI the X layout drives goes off, the screen kept
    screen, outs = W.parse_xrandr(PP_DARK.replace(
        "HDMI-0 connected primary (normal", "HDMI-0 connected primary 2560x1440+0+0 (normal"))
    assert W.decide(screen, outs, W.effective_fixed([], outs, hdmi_off=True))[-3:] == [
        "--output", "HDMI-0", "--off"]
    # the stream role keeps HDMI to X; an unclassifiable config stays unclassifiable
    assert W.effective_fixed([], outs, hdmi_off=False) == []
    assert W.effective_fixed(None, outs, hdmi_off=True) is None
    assert W.effective_fixed(["HDMI-0"], outs, hdmi_off=True).count("HDMI-0") == 1


def test_the_policy_comes_from_the_environment_the_unit_reads():
    assert W.hdmi_off_from_env({"STRIH_DISPLAY_WATCH_HDMI": "off"}) is True
    assert W.hdmi_off_from_env({"STRIH_DISPLAY_WATCH_HDMI": "operator"}) is False
    assert W.hdmi_off_from_env({}) is False
    u = UNIT.read_text()
    assert "EnvironmentFile=-%h/.config/strih-display-watch.env" in u.splitlines()


def test_a_watch_on_the_strih_role_never_puts_hdmi_on_the_desktop(tmp_path):
    rig = Rig(PP_DARK)
    logs = []
    w = W.Watch(state_file=str(tmp_path / "w.json"), run=rig.run, log=logs.append,
                config_text=lambda: "", hdmi_off=True)
    w.pass_once(now=100.0)
    assert rig.applied == [["--fb", "1920x1080", "--noprimary"]]
    assert any("HDMI never on the desktop" in ln for ln in logs), logs


def test_step_16d_writes_the_role_policy_and_restarts_a_running_watch_on_a_change():
    lib = SESSION_LIB.read_text()
    body = lib.split("strih_session_apps_install() {", 1)[1].split("\n}\n", 1)[0]
    assert "strih_display_watch_env_put" in body
    assert body.index("strih_display_watch_env_put") < body.index('"$user" try-restart')
