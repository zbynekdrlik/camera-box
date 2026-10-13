"""issue 1424 -- the fixed HDMI output's audio leg: the box side (scripts/lib/drm-output-audio.sh).

Owner 13.10.2026: the in-OBS HDMI output plays the OBS program mix on its OWN connector's HDMI audio,
written by the vendored libobs module straight into that connector's ALSA hw PCM ("Ziadnych milion
vrstiev po ceste"). For OBS to own the PCM, PipeWire must never hold an HDMI/DP PCM on such a box:
ONE WirePlumber rule disables every ALSA node whose api.alsa.path is an `hdmi:` path. Setup writes it
(setup-strih through strih_drm_output_provision, setup-imag), verify grades the rule, PipeWire's live
node list and the module's outcome line.

Every value reaches bash as a positional argument, never inside the script text; every helper runs
under the caller's real `set -euo pipefail`. Tier-0: python + bash subprocesses only.
"""
import json
import os
import pathlib
import subprocess

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "drm-output-audio.sh"
_STRIH_LIB = _ROOT / "scripts" / "lib" / "strih-drm-output.sh"
_VERIFY = _ROOT / "scripts" / "verify-strih.sh"
_SETUP = _ROOT / "scripts" / "setup-strih.sh"
_SETUP_IMAG = _ROOT / "scripts" / "setup-imag.sh"
_AUDIO_C = _ROOT / "vendor" / "obs-studio" / "libobs" / "obs-drm-output-audio.c"

WP_05 = "wireplumber\nCompiled with libwireplumber 0.5.13\nLinked with libwireplumber 0.5.13\n"
WP_04 = "wireplumber\nCompiled with libwireplumber 0.4.17\nLinked with libwireplumber 0.4.17\n"


def _bash(body, *args, env=None, path_prefix=None):
    script = "set -euo pipefail\n" + '. "$1"\nshift\n' + body + "\n"
    e = dict(os.environ, LC_ALL="C.UTF-8")
    if env:
        e.update(env)
    if path_prefix:
        e["PATH"] = f"{path_prefix}:{e['PATH']}"
    return subprocess.run(
        ["bash", "-c", script, "harness", str(_LIB), *args],
        capture_output=True,
        text=True,
        env=e,
        check=False,
    )


def _fake_wireplumber(tmp_path, text):
    d = tmp_path / "bin"
    d.mkdir(exist_ok=True)
    (d / "version.txt").write_text(text)
    w = d / "wireplumber"
    w.write_text(f'#!/bin/bash\ncat "{d}/version.txt"\n')
    w.chmod(0o755)
    return str(d)


@pytest.mark.parametrize(
    "text,want",
    [(WP_05, "conf"), (WP_04, "lua"), ("Linked with libwireplumber 1.0.2\n", "conf"), ("", "unknown"),
     ("wireplumber: command not found", "unknown")],
)
def test_flavour_follows_the_wireplumber_version(text, want):
    r = _bash('drm_output_audio_wp_flavour "$1"', text)
    assert r.returncode == 0, r.stderr
    assert r.stdout == want


def test_rule_paths_are_the_system_drop_in_dirs():
    r = _bash('drm_output_audio_wp_rule_path conf "$1"; echo; drm_output_audio_wp_rule_path lua "$1"', "/R")
    assert r.stdout.splitlines() == [
        "/R/etc/wireplumber/wireplumber.conf.d/51-camera-box-drm-output-hdmi.conf",
        "/R/etc/wireplumber/main.lua.d/51-camera-box-drm-output-hdmi.lua",
    ]
    assert _bash("drm_output_audio_wp_rule_path unknown || echo REFUSED").stdout == "REFUSED\n"


def test_the_rule_disables_every_hdmi_alsa_node_and_nothing_else():
    conf = _bash("drm_output_audio_wp_rule conf").stdout
    assert "monitor.alsa.rules" in conf
    assert '{ api.alsa.path = "~hdmi:.*" }' in conf
    assert "node.disabled = true" in conf
    # the match key and value are the ones PipeWire puts on an HDMI node (strih-pp, 13.10.2026:
    # alsa_output.pci-0000_0a_00.1.hdmi-stereo carries api.alsa.path = "hdmi:0"), and an analog or
    # USB node's path never starts with hdmi:
    lua = _bash("drm_output_audio_wp_rule lua").stdout
    assert '{ "api.alsa.path", "matches", "hdmi:*" }' in lua
    assert '["node.disabled"] = true' in lua
    # never a device-level rule, never a card name: the operator's analog/USB audio stays
    for t in (conf, lua):
        assert "device.disabled" not in t and "card.name" not in t


def test_provision_writes_the_rule_once_and_never_restarts_the_session(tmp_path):
    fake = _fake_wireplumber(tmp_path, WP_05)
    root = tmp_path / "root"
    r = _bash('drm_output_audio_wp_provision "$1"', str(root), path_prefix=fake)
    assert r.returncode == 0, r.stderr
    path = root / "etc/wireplumber/wireplumber.conf.d/51-camera-box-drm-output-hdmi.conf"
    assert path.read_text() == _bash("drm_output_audio_wp_rule conf").stdout
    assert oct(path.stat().st_mode & 0o777) == "0o644"
    assert "wrote" in r.stdout and "next audio session start" in r.stdout
    again = _bash('drm_output_audio_wp_provision "$1"', str(root), path_prefix=fake)
    assert "already current" in again.stdout
    lib = _LIB.read_text()
    assert "restart wireplumber" not in lib.split("drm_output_audio_wp_provision()")[1].split("\n}\n")[0]


def test_provision_without_a_wireplumber_version_writes_nothing_and_says_so(tmp_path):
    fake = _fake_wireplumber(tmp_path, "")
    root = tmp_path / "root"
    r = _bash('drm_output_audio_wp_provision "$1"; echo CONTINUES', str(root), path_prefix=fake)
    assert r.returncode == 0 and "CONTINUES" in r.stdout
    assert "NOT written" in r.stderr
    assert not (root / "etc").exists()


def test_provision_that_cannot_write_warns_and_never_stops_setup(tmp_path):
    fake = _fake_wireplumber(tmp_path, WP_05)
    blocked = tmp_path / "ro"
    blocked.mkdir()
    blocked.chmod(0o555)
    try:
        r = _bash('drm_output_audio_wp_provision; echo CONTINUES', env={"DRM_OUTPUT_AUDIO_WP_ROOT": str(blocked)},
                  path_prefix=fake)
    finally:
        blocked.chmod(0o755)
    assert r.returncode == 0 and "CONTINUES" in r.stdout, r.stderr
    assert "could not write" in r.stderr
    assert "wrote" not in r.stdout


def _node(path, name):
    return {"type": "PipeWire:Interface:Node", "info": {"props": {"api.alsa.path": path, "node.name": name}}}


def test_hdmi_node_count_reads_pw_dump():
    dump = [
        _node("hdmi:0", "alsa_output.pci-0000_0a_00.1.hdmi-stereo"),
        _node("front:3", "alsa_output.usb-ARTURIA_MiniFuse_4"),
        {"type": "PipeWire:Interface:Device", "info": {"props": {"api.alsa.path": "hdmi:0"}}},
        _node("hdmi:1,1", "alsa_output.pci-0000_0a_00.1.hdmi-stereo-extra1"),
    ]
    r = _bash("drm_output_audio_hdmi_node_count <<<\"$1\"", json.dumps(dump))
    assert r.stdout.strip() == "2"
    assert _bash("drm_output_audio_hdmi_node_count <<<\"$1\"", "[]").stdout.strip() == "0"
    assert _bash("drm_output_audio_hdmi_node_count <<<\"$1\"", "not json").stdout.strip() == "?"


OPEN = ("info: drm-output audio: pcm hw:0,3 card 0 device 3 delay 0.0 ms (on 'HDMI-0'; audio 96.2 ms behind "
        "the picture, present 17.1 ms, pcm delay 64.0 ms)")
MATCHED = ("info: drm-output audio: 'HDMI-0' matched hw:0,3 (48000 Hz, S32_LE, IEC958 switch on) -- mix 0 "
           "connected (muted while the view is multiview)")
LOST = ("warning: drm-output audio: 'HDMI-0' hw:0,3 no longer carries the connector's monitor (an HDMI replug "
        "or a re-mode moved the PCM map) -- muted")
BACK = "info: drm-output audio: 'HDMI-0' hw:0,3 carries the connector's monitor again"
WRITEFAIL = "warning: drm-output audio: 'HDMI-0' write to hw:0,3 failed (No such device)"
NOAUDIO = "warning: drm-output audio: NO AUDIO on 'HDMI-1' -- no PCM's ELD matches the connector's EDID; never a fallback"


@pytest.mark.parametrize(
    "lines,want",
    [([OPEN], "open"), ([NOAUDIO], "no-audio"), ([OPEN, NOAUDIO], "no-audio"), ([NOAUDIO, OPEN], "open"),
     (["info: drm-output: ACTIVE"], "none"), ([], "none"),
     # the Multiview view places no block, so only the resolver's line is there (review R2)
     ([MATCHED], "open"), ([MATCHED, NOAUDIO], "no-audio"),
     # review round 2 N1: the watcher's mute and a failed write are no-audio, the last line wins
     ([OPEN, LOST], "no-audio"), ([OPEN, LOST, BACK], "open"), ([MATCHED, WRITEFAIL], "no-audio")],
)
def test_log_state_takes_the_last_outcome(lines, want):
    text = "\n".join(lines) + "\n"
    assert _bash('drm_output_audio_log_state <<<"$1"', text).stdout.strip() == want
    if lines and want != "none":
        assert _bash('drm_output_audio_log_line <<<"$1"', text).stdout.strip() == lines[-1]


def test_log_markers_are_the_strings_the_module_writes():
    c = _AUDIO_C.read_text()
    assert "drm-output audio: pcm hw:%d,%d card %d device %d delay" in c
    assert "drm-output audio: NO AUDIO on '%s'" in c
    assert "drm-output audio: '%s' matched hw:%d,%d" in c
    assert "hw:%d,%d no longer carries the connector's monitor" in c
    assert "hw:%d,%d carries the connector's monitor again" in c
    assert "drm-output audio: '%s' write to hw:%d,%d failed" in c


@pytest.mark.parametrize(
    "rule,nodes,log,token,rc",
    [
        ("ok", "0", "open", "ok", 0),
        ("unknown", "0", "open", "wp-unknown", 1),
        ("missing", "0", "open", "rule-missing", 1),
        ("drift", "0", "open", "rule-drift", 1),
        ("ok", "2", "open", "pipewire-holds-hdmi", 1),
        ("ok", "0", "no-audio", "no-audio", 1),
        ("ok", "0", "none", "no-line", 1),
        ("ok", "?", "open", "pipewire-unread", 2),
        ("ok", "?", "no-audio", "no-audio", 1),
    ],
)
def test_verdict_truth_table(rule, nodes, log, token, rc):
    r = _bash('drm_output_audio_verdict "$1" "$2" "$3" || echo "RC=$?"', rule, nodes, log)
    out = r.stdout
    assert out.startswith(token)
    assert (f"RC={rc}" in out) if rc else ("RC=" not in out)


def test_verify_report_grades_through_the_callers_ok_bad_note(tmp_path):
    fake = _fake_wireplumber(tmp_path, WP_05)
    pw = pathlib.Path(fake) / "pw-dump"
    pw.write_text("#!/bin/bash\necho '[]'\n")
    pw.chmod(0o755)
    root = tmp_path / "root"
    log = tmp_path / "obs.txt"
    log.write_text(OPEN + "\n")
    body = (
        'ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }; note() { echo "NOTE $1"; }\n'
        'drm_output_audio_wp_provision "$3" >/dev/null\n'
        'drm_output_audio_verify_report "$1" "$2" "$4" "$3"'
    )
    user = subprocess.run(["id", "-un"], capture_output=True, text=True, check=True).stdout.strip()
    r = _bash(body, str(tmp_path), user, str(root), str(log), path_prefix=fake)
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("PASS HDMI audio:"), r.stdout
    assert "pcm hw:0,3" in r.stdout
    pw.write_text("#!/bin/bash\n" + "echo '" + json.dumps([_node("hdmi:0", "x")]) + "'\n")
    r = _bash(body, str(tmp_path), user, str(root), str(log), path_prefix=fake)
    assert r.stdout.startswith("FAIL HDMI audio: PipeWire still has 1 HDMI node(s)"), r.stdout


def test_setup_and_verify_reach_the_lib_without_growing_the_capped_scripts():
    strih_lib = _STRIH_LIB.read_text()
    assert '. "$(dirname "${BASH_SOURCE[0]}")/drm-output-audio.sh"' in strih_lib
    prov = strih_lib.split("strih_drm_output_provision() {")[1]
    assert "drm_output_audio_wp_provision" in prov, "setup-strih step 6 writes the rule"
    verify = _VERIFY.read_text()
    ok_line = [ln for ln in verify.splitlines() if ln.strip().startswith("ok)") and "HDMI output:" in ln]
    assert len(ok_line) == 1 and "drm_output_audio_verify_report" in ok_line[0], "graded when the output is live"
    assert len(verify.splitlines()) <= 1000 and len(_SETUP.read_text().splitlines()) <= 1000
    imag = _SETUP_IMAG.read_text()
    assert '. "$_RG_HERE/lib/drm-output-audio.sh"' in imag
    assert imag.count("drm_output_audio_wp_provision") == 1
