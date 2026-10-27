"""issue 1482 -- the SNV rig's test feed (optical | direct) built into rig-mode TEST / EVENT.

Owner 25.10.2026: cam2's HDMI now goes straight into the splitter (no camera filming its monitor), the sound is
taken from cam2's jack, "a nech to mas zabudovane do test/event modu korektne". ROZHODNUTE: in the direct feed cam2
runs no intercom (its USB jack is the intercom headset device and now carries the QPSK marker).
These tests run the lib's emitted remote text against a temp root and pin the rig-mode / recording-e2e wiring.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "rig-test-feed.sh"
CTS = REPO / "scripts" / "lib" / "camera-test-settings.sh"
RIG = REPO / "scripts" / "rig-mode.sh"
E2E = REPO / "scripts" / "recording-e2e.sh"
APLAY = ("card 0: C4K [CAM LINK 4K], device 0: USB Audio [USB Audio]\n"
         "card 1: HID [USB Audio and HID], device 0: USB Audio [USB Audio]\n"
         "card 2: PCH [HDA Intel PCH], device 3: HDMI 0 [GSV2K6 DEMO]\n")


def _lib(body, env=None):
    e = {"PATH": os.environ["PATH"], "HOME": os.environ.get("HOME", "/tmp")}
    e.update(env or {})
    return subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\n' + body, "h", str(LIB)],
                          capture_output=True, text=True, env=e, timeout=30)


def test_direct_is_the_default_and_a_bad_value_is_refused():
    assert _lib("rig_test_feed").stdout == "direct"
    assert _lib("rig_test_feed", {"RIG_TEST_FEED": "optical"}).stdout == "optical"
    r = _lib("rig_test_feed", {"RIG_TEST_FEED": "camera"})
    assert r.returncode == 2 and "neither optical nor direct" in r.stderr


def test_the_marker_device_follows_the_feed():
    assert _lib("rig_test_feed_marker_device").stdout == "hw:CARD=HID,DEV=0"
    assert _lib("rig_test_feed_marker_device", {"RIG_TEST_FEED": "optical"}).stdout == "hw:CARD=PCH,DEV=3"


def test_the_jack_check_reads_aplay():
    ok = _lib('rig_test_feed_jack_present "$X" hw:CARD=HID,DEV=0', {"X": APLAY})
    gone = _lib('rig_test_feed_jack_present "$X" hw:CARD=HID,DEV=0', {"X": APLAY.replace("HID [", "XYZ [")})
    hdmi_only = _lib('rig_test_feed_jack_present "$X" hw:CARD=HID,DEV=1', {"X": APLAY})
    assert ok.returncode == 0 and gone.returncode != 0 and hdmi_only.returncode != 0


def _run_remote(tmp_path, text, aplay=APLAY):
    b = tmp_path / "bin"
    b.mkdir(exist_ok=True)
    (b / "aplay").write_text("#!/bin/sh\ncat <<'X'\n%sX\n" % aplay)
    (b / "systemctl").write_text('#!/bin/sh\necho "systemctl $*" >> %s/calls\n' % tmp_path)
    for f in ("aplay", "systemctl"):
        (b / f).chmod(0o755)
    return subprocess.run(["bash", "-c", "set -e\n" + text], capture_output=True, text=True, timeout=30,
                          env={"PATH": "%s:/usr/bin:/bin" % b})


def _env(tmp_path, feed="direct"):
    return {"RIG_TEST_FEED": feed,
            "RIG_TEST_FEED_PAINTER_DROPIN": str(tmp_path / "run/cam2-painter.service.d/zz-rig-direct-marker.conf"),
            "RIG_TEST_FEED_INTERCOM_DROPIN": str(tmp_path / "run/camera-box.service.d/zz-rig-direct-intercom-off.conf")}


def test_test_mode_writes_both_drop_ins_and_event_removes_them(tmp_path):
    env = _env(tmp_path)
    for fn in ("rig_test_feed_intercom_dropin_cmds", "rig_test_feed_painter_dropin_cmds"):
        r = _run_remote(tmp_path, _lib(fn, env).stdout)
        assert r.returncode == 0, r.stderr
    painter = Path(env["RIG_TEST_FEED_PAINTER_DROPIN"]).read_text()
    intercom = Path(env["RIG_TEST_FEED_INTERCOM_DROPIN"]).read_text()
    assert painter == "[Service]\nEnvironment=FRAME_PROBE_AUDIO_MARKER_DEVICE=hw:CARD=HID,DEV=0\n"
    assert intercom == "[Service]\nEnvironment=CAMERA_BOX_INTERCOM=off\n"
    assert "systemctl daemon-reload" in (tmp_path / "calls").read_text()
    r = _run_remote(tmp_path, _lib("rig_test_feed_painter_dropin_clear_cmds", env).stdout)
    assert r.returncode == 0, r.stderr
    assert not Path(env["RIG_TEST_FEED_PAINTER_DROPIN"]).exists()
    assert not Path(env["RIG_TEST_FEED_INTERCOM_DROPIN"]).exists()


def test_a_missing_jack_fails_the_painter_step_and_writes_nothing(tmp_path):
    env = _env(tmp_path)
    r = _run_remote(tmp_path, _lib("rig_test_feed_painter_dropin_cmds", env).stdout, aplay="card 2: PCH [x], device 3: HDMI 0 [y]\n")
    assert r.returncode == 1 and "not in cam2's aplay -l" in r.stderr
    assert not Path(env["RIG_TEST_FEED_PAINTER_DROPIN"]).exists()


def test_the_optical_feed_removes_the_drop_ins(tmp_path):
    direct, optical = _env(tmp_path), _env(tmp_path, "optical")
    for fn in ("rig_test_feed_intercom_dropin_cmds", "rig_test_feed_painter_dropin_cmds"):
        _run_remote(tmp_path, _lib(fn, direct).stdout)
    for fn in ("rig_test_feed_intercom_dropin_cmds", "rig_test_feed_painter_dropin_cmds"):
        r = _run_remote(tmp_path, _lib(fn, optical).stdout)
        assert r.returncode == 0, r.stderr
    assert not Path(direct["RIG_TEST_FEED_PAINTER_DROPIN"]).exists()
    assert not Path(direct["RIG_TEST_FEED_INTERCOM_DROPIN"]).exists()


def test_the_e2e_enforce_skips_the_test_camera_in_the_direct_feed_only(tmp_path):
    def run(feed):
        script = ('set -euo pipefail\n. "$1"\n. "$2"\n'
                  'camera_test_settings_enforce "$3" 10.0.0.2 10.0.0.4 pw "cam5=10.0.0.9" "cam2=10.0.0.8" && echo RC0')
        env = {"PATH": "/usr/bin:/bin", "HOME": str(tmp_path), "RIG_TEST_FEED": feed}
        # no ssh / camera here: the optical feed reaches the real enforce, which must not print the skip line
        return subprocess.run(["bash", "-c", script, "h", str(LIB), str(CTS), str(REPO / "scripts")],
                              capture_output=True, text=True, env=env, timeout=60)
    d = run("direct")
    assert d.returncode == 0 and "SKIPPED: RIG_TEST_FEED=direct" in d.stdout and "RC0" in d.stdout
    o = run("optical")
    assert "SKIPPED: RIG_TEST_FEED=direct" not in o.stdout + o.stderr


def test_rig_mode_wiring():
    s = RIG.read_text()
    assert s.count('. "$RIG_MODE_DIR/lib/rig-test-feed.sh"') == 1
    # TEST: the intercom drop-in is written in the camera-box switch, before its restart
    launch = s[s.index("painter_launch_remote() {"):s.index("\n}\n", s.index("painter_launch_remote() {"))]
    assert launch.index("$(rig_test_feed_intercom_dropin_cmds)") < launch.index("systemctl restart camera-box")
    # TEST: the painter drop-in right before the steady-state handoff
    t = s[s.index("do_test() {"):s.index("do_event() {")]
    assert t.index('cam_ssh "$(rig_test_feed_painter_dropin_cmds)"') \
        < t.index('cam_ssh "$(cam2_painter_steady_state_handoff_cmds')
    # EVENT: both drop-ins go BEFORE the cam-side restore restarts camera-box
    e = s[s.index("do_event() {"):]
    assert e.index('cam_ssh "$(rig_test_feed_painter_dropin_clear_cmds)"') < e.index('cam_ssh "$(painter_stop_remote')
    # the marker device resolution and its post-launch check know the direct feed
    res = s[s.index("resolve_marker_device() {"):s.index("verify_marker_device_monitor() {")]
    assert 'if [ "$(rig_test_feed)" = direct ]; then' in res
    ver = s[s.index("verify_marker_device_monitor() {"):s.index("do_test() {")]
    assert "rig_test_feed_jack_present" in ver


def test_recording_e2e_follows_the_feed():
    s = E2E.read_text()
    assert s.count('. "$HERE/lib/rig-test-feed.sh"') == 1
    assert s.index('. "$HERE/lib/rig-test-feed.sh"') < s.index("camera_test_settings_enforce ")
    assert 'AV_SYNC_MARKER_DEVICE="${AV_SYNC_MARKER_DEVICE:-$(rig_test_feed_marker_device)}"' in s
    assert 'AV_RESTART_MARKER_DEVICE="${AV_RESTART_MARKER_DEVICE:-$(rig_test_feed_marker_device)}"' in s


def test_the_burn_unit_gets_the_intercom_off_env_in_the_direct_feed_only():
    # The E2E's cam2 burn camera-box runs as a transient systemd-run unit, which never reads camera-box.service's
    # drop-ins: without this env its intercom opens the jack and the QPSK marker fails "Device or resource busy"
    # (E2E 40222056776 attempt 2, 25.10.2026).
    assert _lib("rig_test_feed_burn_setenv").stdout == "--setenv=CAMERA_BOX_INTERCOM=off "
    assert _lib("rig_test_feed_burn_setenv", {"RIG_TEST_FEED": "optical"}).stdout == ""
    assert _lib("rig_test_feed_burn_setenv", {"RIG_TEST_FEED": "camera"}).returncode == 2


def test_recording_e2e_gives_the_cam2_burn_unit_the_intercom_env():
    s = E2E.read_text()
    line = [ln for ln in s.splitlines() if ln.strip().startswith('if [ "$_cn" = "cam2" ]; then _cnodisplay_setenv=')]
    assert len(line) == 1
    assert "$(rig_test_feed_burn_setenv)" in line[0]
    assert s.index('. "$HERE/lib/rig-test-feed.sh"') < s.index("$(rig_test_feed_burn_setenv)")


def test_the_production_av_pin_is_never_tuned_from_the_direct_feed():
    # The direct feed has no camera in its video path (production and the optical feed do): two of three direct-feed
    # E2E runs (40233177561, 40246131760) read -51 / -52 ms where every optical run read within +-18 ms, and each time
    # the #856 cleanup apply moved the production stream pin 1087 -> 1120 ms. Restored by hand, read-back verified.
    hold = _lib("rig_test_feed_av_apply_hold")
    assert hold.returncode == 0 and "issue 1482" in hold.stdout and "direct" in hold.stdout
    assert _lib("rig_test_feed_av_apply_hold", {"RIG_TEST_FEED": "optical"}).stdout == ""


def test_recording_e2e_holds_the_av_apply_in_the_direct_feed():
    s = E2E.read_text()
    call = '_avs_feed_hold="$(rig_test_feed_av_apply_hold || true)"'
    assert s.count(call) == 1
    guard = s.index('_avs_hold="$(av_sync_apply_guard_decide')
    apply = s.index('[cleanup] #856: applying')
    assert s.index('AV_SYNC_PROPOSED_OFFSET_MS="$AV_SYNC_APPLY_OFFSET_MS"') < s.index(call) < guard < apply
    block = s[s.index(call):guard]
    assert 'AV_SYNC_APPLY_OFFSET_MS=""' in block and "AV_SYNC_HELD_REASON=" in block


def test_the_av_gate_expects_the_direct_feeds_own_offset():
    # Every direct-feed measurement since the jack switch (8 E2E + soak windows, 25.-27.10.2026): -38..-52 ms,
    # median -46.5, uniform over the cameras (ROZHODNUTE on the ticket, 27.10.2026). The camera+monitor feed keeps 0.
    assert _lib("rig_test_feed_av_expected_ms").stdout == "-45"
    assert _lib("rig_test_feed_av_expected_ms", {"RIG_TEST_FEED": "optical"}).stdout == "0"


def test_recording_e2e_expects_the_feeds_av_offset_unless_given():
    s = E2E.read_text()
    default = 'AV_EXPECTED_MS="${AV_EXPECTED_MS:-0}"'
    shift = '[ -z "$AV_EXPECTED_MS_FROM_ENV" ] && AV_EXPECTED_MS="$(rig_test_feed_av_expected_ms)"'
    assert s.count(shift) == 1
    assert s.index('AV_EXPECTED_MS_FROM_ENV="${AV_EXPECTED_MS+set}"') < s.index(default) < s.index(shift)
    assert s.index(shift) < s.index('VERDICT_ARGS+=(--av-expected-ms "$AV_EXPECTED_MS")')
