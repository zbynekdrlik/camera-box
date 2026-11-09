#!/usr/bin/env python3
"""issue 1452 (shading#6, owner 9.11.2026) -- the shading v0.2.x REST relay in camera-box.

1. The relay pin is chosen PER BOX: scripts/shading-release-boxes.txt names the boxes on the REST pin
   (scripts/shading-release-rest.pin, v0.2.x, no gphoto2 fallback); every other box keeps the fleet
   default scripts/shading-release.pin. deploy-fleet, bkshading-deploy-relay.sh (+ its own-hostname
   check), setup-device, verify-device (ao) and the handover grade a box against ITS pin.
2. The camera's USB-C network link file (`05-bkshading-camera.network`, the contract's text byte for
   byte): setup-device, the SBC, the live-box apply program, verify-device (bb).
3. The issue-1238 preflight reads the REST relay's focusNorm.
The E2E [0/8] REST step itself: test_camera_test_settings_1371.py + test_camera_prod_exposure_restore_1371.py.
"""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "shading-release.sh")
LINK = os.path.join(REPO, "scripts", "lib", "bkshading-camera-link.sh")
PREFLIGHT = os.path.join(REPO, "scripts", "lib", "bkshading-preflight.sh")
DEPLOY_RELAY = os.path.join(REPO, "scripts", "bkshading-deploy-relay.sh")
VERIFY = os.path.join(REPO, "scripts", "verify-device.sh")
SETUP = os.path.join(REPO, "scripts", "setup-device.sh")
SBC = os.path.join(REPO, "scripts", "bkshading-provision-sbc.sh")
REST_CONTRACT = os.path.join(REPO, "tests", "fixtures", "shading_pinned_rest", "CONTRACT.md")

from ro_window_fakes_1407 import make_box, run_text, log, root  # noqa: E402


def _bash(script, *args, env=None):
    e = dict(os.environ)
    e.pop("SHADING_RELEASE_PIN_FILE", None)
    e.pop("SHADING_RELEASE_BOXES_FILE", None)
    e.update(env or {})
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + script, "t", *args],
                          capture_output=True, text=True, env=e)


def _read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


# ---- 1. the per-box pin ------------------------------------------------------------------------- #

def test_the_map_entry_parser_skips_comments_and_matches_a_box_case_and_domain_free():
    text = "# a comment\ncam3 shading-release-rest.pin  # trailing\n\nCAM5 other.pin\ncam3 second.pin\n"
    for box, want in (("cam3", "shading-release-rest.pin"), ("CAM3", "shading-release-rest.pin"),
                      ("cam3.lan", "shading-release-rest.pin"), ("cam5", "other.pin"), ("cam2", ""),
                      ("", ""), ("ca m3", "")):
        r = _bash('. "$1"; shading_pin_map_entry "$2" "$3"', LIB, text, box)
        assert r.returncode == 0 and r.stdout.strip() == want, (box, r.stdout, r.stderr)


def test_the_tree_puts_cam3_and_cam5_on_the_rest_pin_and_every_other_box_on_the_default():
    for box, want in (("cam3", "shading-release-rest.pin"), ("CAM5", "shading-release-rest.pin"),
                      ("cam1", "shading-release.pin"), ("cam7", "shading-release.pin"),
                      ("handheld-1", "shading-release.pin"), ("", "shading-release.pin")):
        r = _bash('. "$1"; shading_pin_file_for_box "$2" "$3"', LIB, REPO, box)
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip() == os.path.join(REPO, "scripts", want), (box, r.stdout)
    r = _bash('. "$1"; shading_pin_version_for_box "$2" cam3; shading_pin_version_for_box "$2" cam7', LIB, REPO)
    assert r.stdout.split() == ["0.2.2", "0.1.2"], r.stdout


def test_a_pin_file_seam_alone_pins_every_box_and_a_map_seam_still_maps(tmp_path):
    seam = tmp_path / "one.pin"
    seam.write_text("SHADING_RELEASE_TAG=v9.9.9\n")
    r = _bash('. "$1"; shading_pin_file_for_box "$2" cam3', LIB, REPO, env={"SHADING_RELEASE_PIN_FILE": str(seam)})
    assert r.stdout.strip() == str(seam)
    m = tmp_path / "boxes.txt"
    m.write_text("cam3 b.pin\n")
    (tmp_path / "b.pin").write_text("SHADING_RELEASE_TAG=v8.8.8\n")
    env = {"SHADING_RELEASE_PIN_FILE": str(seam), "SHADING_RELEASE_BOXES_FILE": str(m)}
    r = _bash('. "$1"; shading_pin_version_for_box "$2" cam3; shading_pin_version_for_box "$2" cam2', LIB, REPO, env=env)
    assert r.stdout.split() == ["8.8.8", "9.9.9"], (r.stdout, r.stderr)


def test_a_map_naming_a_missing_or_odd_pin_file_fails_by_name(tmp_path):
    m = tmp_path / "boxes.txt"
    for entry in ("cam3 nope.pin", "cam3 ../../etc/passwd", "cam3 x.txt"):
        m.write_text(entry + "\n")
        r = _bash('. "$1"; shading_pin_file_for_box "$2" cam3', LIB, REPO, env={"SHADING_RELEASE_BOXES_FILE": str(m)})
        assert r.returncode == 1 and "no pin file" in r.stderr, (entry, r.stdout, r.stderr)


def test_the_merged_pin_of_a_box_is_read_from_a_ref(tmp_path):
    repo = tmp_path / "r"
    (repo / "scripts").mkdir(parents=True)
    (repo / "scripts" / "shading-release.pin").write_text("SHADING_RELEASE_TAG=v0.1.2\n")
    (repo / "scripts" / "shading-release-rest.pin").write_text("SHADING_RELEASE_TAG=v0.2.2\n")
    (repo / "scripts" / "shading-release-boxes.txt").write_text("cam3 shading-release-rest.pin\n")
    git = ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t"]
    subprocess.run(git[:3] + ["init", "-q"], check=True)
    subprocess.run(git + ["add", "-A"], check=True)
    subprocess.run(git + ["commit", "-qm", "x"], check=True)
    r = _bash('. "$1"; shading_pin_version_for_box_at_ref "$2" HEAD cam3; shading_pin_version_for_box_at_ref "$2" HEAD cam1',
              LIB, str(repo))
    assert r.stdout.split() == ["0.2.2", "0.1.2"], (r.stdout, r.stderr)


def test_every_relay_consumer_grades_a_box_against_its_own_pin():
    setup = _read(SETUP)
    assert 'bkshading_relay_provision_fetch_binary "$RELAY_PLAN" "$RELAY_DL_DIR" "$DEVICE_NAME"' in setup
    assert 'bkshading_relay_pin_version "$CAMERA_NAME"' in _read(VERIFY)
    fleet = _read(os.path.join(REPO, "scripts", "deploy-fleet.sh"))
    assert '--host "$ip" --box "$cam" --binary "$RELAY_BIN"' in fleet
    assert fleet.count('--box "$cam"') == 2
    hand = _read(os.path.join(REPO, "scripts", "rig-dev-handover-check.sh"))
    assert 'shading_pin_version_for_box "$HERE/.." "$label"' in hand


def _fake_relay(path, version):
    path.write_text("#!/bin/bash\necho 'bkshading-relay %s'\n" % version)
    path.chmod(0o755)
    return path


def test_deploy_fleet_brings_a_mapped_box_onto_its_own_pin(tmp_path):
    from shading_release_fake_1487 import pin_text, _sha
    import test_deploy_fleet_relay_1452 as fleet
    rest = _fake_relay(tmp_path / "rest-relay", "9.9.8-rest")
    base = tmp_path / "fake-shading-release"
    d = base / "v9.9.8-rest"
    d.mkdir(parents=True)
    (d / "bkshading-relay-linux-amd64").write_bytes(rest.read_bytes())
    (d / "bkshading-relay-linux-amd64.sha256").write_text("%s  bkshading-relay-linux-amd64\n" % _sha(rest))
    mapdir = tmp_path / "map"
    mapdir.mkdir()
    (mapdir / "rest.pin").write_text(pin_text("9.9.8-rest", {"bkshading-relay-linux-amd64": _sha(rest)}))
    (mapdir / "boxes.txt").write_text("cam2 rest.pin\n")
    proc, box = fleet._deploy_with_relay(tmp_path, ["--binary", str(fleet._camera_box_artifact(tmp_path))],
                                         SHADING_RELEASE_BOXES_FILE=str(mapdir / "boxes.txt"))
    out = proc.stdout + proc.stderr
    assert proc.returncode == 0, out
    assert fleet._live(box, "bkshading-relay").read_bytes() == rest.read_bytes(), out
    assert "cam2=9.9.8-rest" in out and "cam2 camera-box=9.9.9-test relay=9.9.8-rest" in fleet._summary(proc)


def _deploy_relay(tmp_path, name, args, env_extra=None, rest_bytes_pinned=False):
    """bkshading-deploy-relay.sh against the 808 fake ssh, whose box names itself `name`."""
    import test_bkshading_deploy_relay_808 as dr
    binary = tmp_path / "relay"
    binary.write_bytes(b"RELAYBYTES")
    sha = subprocess.run(["sha256sum", str(binary)], capture_output=True, text=True, check=True).stdout.split()[0]
    env, logf = dr._fake_deploy_env(str(tmp_path), sha)
    ssh = tmp_path / "fake-ssh"
    body = ssh.read_text()
    body = body.replace('  *"uname -n"*) printf "cam9\\n" ;;\n', "")
    body = body.replace('case "$cmd" in\n', 'case "$cmd" in\n  *"uname -n"*) printf "%s\\n" ;;\n' % name)
    ssh.write_text(body)
    pin = tmp_path / "default.pin"
    pin.write_text("SHADING_RELEASE_TAG=v0.1.2\nSHADING_RELAY_LINUX_AMD64_SHA256=%s\n" % sha)
    rest = tmp_path / "rest.pin"
    rest.write_text("SHADING_RELEASE_TAG=v0.2.2\nSHADING_RELAY_LINUX_AMD64_SHA256=%s\nSHADING_RELAY_TRANSPORT=rest\n"
                    % (sha if rest_bytes_pinned else "b" * 64))
    (tmp_path / "boxes.txt").write_text("cam3 rest.pin\n")
    env.update({"SHADING_RELEASE_PIN_FILE": str(pin), "SHADING_RELEASE_BOXES_FILE": str(tmp_path / "boxes.txt")})
    env.update(env_extra or {})
    e = dict(os.environ)
    e.update(env)
    r = subprocess.run(["bash", DEPLOY_RELAY, "--host", "10.0.0.9", "--binary", str(binary), *args],
                       capture_output=True, text=True, env=e)
    return r, open(logf).read() if os.path.exists(logf) else ""


def test_deploy_relay_refuses_a_box_whose_own_name_maps_to_another_pin(tmp_path):
    # a cam3 deployed without --box would get the fleet default; its own hostname says rest.pin
    r, calls = _deploy_relay(tmp_path, "cam3", [])
    assert r.returncode == 1, r.stdout + r.stderr
    assert "names itself 'cam3', whose pin is rest.pin" in r.stderr and "pass --box cam3" in r.stderr
    assert "remount" not in calls and "SCP" not in calls and "systemctl stop" not in calls, calls


def test_deploy_relay_with_box_takes_that_boxs_pin_and_refuses_bytes_of_another(tmp_path):
    # --box cam3 = the REST pin, whose sha the fake binary is not: only the pinned bytes deploy
    r, calls = _deploy_relay(tmp_path, "cam3", ["--box", "cam3"])
    assert r.returncode == 1 and "is not the pinned" in r.stderr, r.stdout + r.stderr
    assert "SCP" not in calls


def test_deploy_relay_on_a_default_box_names_its_pin_and_deploys(tmp_path):
    r, calls = _deploy_relay(tmp_path, "cam7", ["--box", "cam7"])
    assert r.returncode == 0, r.stdout + r.stderr
    assert "box 10.0.0.9 names itself 'cam7': pin default.pin" in r.stdout
    assert "SCP" in calls
    # the name is read BEFORE anything changes on the box
    assert calls.index("uname -n") < calls.index("remount,rw")


def test_deploy_relay_on_a_rest_pin_puts_the_camera_link_in_place_after_the_swap(tmp_path):
    rest = tmp_path / "rest.pin"
    r, calls = _deploy_relay(tmp_path, "cam3", ["--box", "cam3"],
                             env_extra={"SHADING_RELEASE_BOXES_FILE": str(tmp_path / "boxes.txt")},
                             rest_bytes_pinned=True)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "the camera link file is in place on 10.0.0.9" in r.stdout
    link_at = calls.index("05-bkshading-camera.network")
    assert calls.index("SCP") < link_at, "the link program runs after the relay swap"
    assert "networkctl reload" in calls[link_at:]
    assert rest.exists()


def test_deploy_relay_on_the_default_pin_never_writes_the_link(tmp_path):
    r, calls = _deploy_relay(tmp_path, "cam7", ["--box", "cam7"])
    assert r.returncode == 0, r.stdout + r.stderr
    assert "05-bkshading-camera.network" not in calls


def test_deploy_relay_refuses_an_unnamed_box_without_box(tmp_path):
    r, calls = _deploy_relay(tmp_path, "", [])
    assert r.returncode == 1 and "no --box was given" in r.stderr, r.stdout + r.stderr
    assert "remount" not in calls and "SCP" not in calls


def test_deploy_relay_dry_run_names_the_pin(tmp_path):
    r, _ = _deploy_relay(tmp_path, "cam7", ["--box", "cam7", "--dry-run"])
    assert r.returncode == 0 and "pin            : default.pin (box 'cam7'" in r.stdout, r.stdout + r.stderr


# ---- 2. the camera link file -------------------------------------------------------------------- #

def _contract_ini():
    text = _read(REST_CONTRACT)
    return re.search(r"```ini\n(.*?)```", text, re.S).group(1)


def test_the_link_file_is_the_rest_contracts_byte_for_byte():
    text = _bash('. "$1"; bkshading_camera_link_network_text', LINK).stdout
    path = _bash('. "$1"; bkshading_camera_link_network_path', LINK).stdout.strip()
    assert text == _contract_ini(), "the contract moved: copy its .network block"
    assert path == "/etc/systemd/network/05-bkshading-camera.network"
    # the cambox LAN rule: a driver AND vendor match, no route, no DNS, never waited for at boot
    for line in ("Driver=cdc_ncm", "Property=ID_VENDOR_ID=1edb", "UseRoutes=no", "UseGateway=no", "UseDNS=no",
                 "RequiredForOnline=no"):
        assert line in text.splitlines()


def test_the_install_writes_once_enables_networkd_and_never_starts_anything(tmp_path):
    sc = tmp_path / "systemctl"
    sc.write_text('#!/bin/bash\necho "$*" >> "%s/sc.log"\n[ "$1" = is-enabled ] && echo disabled\nexit 0\n' % tmp_path)
    sc.chmod(0o755)
    env = {"BKSHADING_CAMERA_LINK_SYSTEMCTL": str(sc)}
    r = _bash('. "$1"; bkshading_camera_link_install "$2"', LINK, str(tmp_path / "root"), env=env)
    assert r.returncode == 0 and "CAMERA_LINK_WRITTEN=1" in r.stdout, r.stdout + r.stderr
    f = tmp_path / "root" / "etc" / "systemd" / "network" / "05-bkshading-camera.network"
    assert f.read_text() == _contract_ini() and oct(f.stat().st_mode & 0o777) == "0o644"
    r = _bash('. "$1"; bkshading_camera_link_install "$2"', LINK, str(tmp_path / "root"), env=env)
    assert "CAMERA_LINK_WRITTEN=0" in r.stdout
    calls = (tmp_path / "sc.log").read_text()
    assert "enable systemd-networkd.service" in calls
    assert not re.search(r"\b(start|restart|reload)\b", calls), calls


def _verdict(block):
    return _bash('. "$1"; bkshading_camera_link_verdict "$2"', LINK, block).stdout


def test_the_verdict_grades_the_file_networkd_and_no_route_or_dns_through_the_camera():
    sha = _bash('. "$1"; bkshading_camera_link_sha', LINK).stdout.strip()
    good = "LINK_NET_SHA=%s\nLINK_NETWORKD=enabled\nLINK_ENX_DEFAULT=0\nLINK_ENX_DNS=0\n" % sha
    assert _verdict(good) == "ok\n"
    assert "missing" in _verdict(good.replace(sha, ""))
    assert "differs" in _verdict(good.replace(sha, "0" * 64))
    assert "systemd-networkd" in _verdict(good.replace("=enabled", "=disabled"))
    assert "default route" in _verdict(good.replace("LINK_ENX_DEFAULT=0", "LINK_ENX_DEFAULT=1"))
    assert "DNS" in _verdict(good.replace("LINK_ENX_DNS=0", "LINK_ENX_DNS=2"))
    assert "empty" in _verdict("")


def _apply(tmp_path, rootmode="ro", present=False):
    box = make_box(tmp_path, root=rootmode)
    nc = box["stub"] / "networkctl"
    nc.write_text("#!/bin/bash\necho \"networkctl $* root=$(cat \"$FAKE_STATE/root\")\" >> \"$FAKE_STATE/log\"\n")
    nc.chmod(0o755)
    for tool in ("hostname", "mkdir"):
        (box["stub"] / tool).symlink_to(shutil.which(tool))
    target = tmp_path / "net" / "05-bkshading-camera.network"
    if present:
        target.parent.mkdir()
        target.write_text(_contract_ini())
        (box["state"] / "enabled-systemd-networkd.service").write_text("enabled\n")
    prog = _bash('. "$1"; bkshading_camera_link_apply_program', LINK,
                 env={"BKSHADING_CAMERA_LINK_PATH": str(target)}).stdout
    return box, target, run_text(box, prog)


def test_the_live_apply_writes_inside_one_verified_window_and_reloads_only_after_the_close(tmp_path):
    box, target, r = _apply(tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    assert target.read_text() == _contract_ini()
    assert root(box) == "ro"
    calls = log(box)
    reload_at = [i for i, c in enumerate(calls) if c.startswith("networkctl reload")]
    assert reload_at and calls[reload_at[0]].endswith("root=ro"), calls
    assert any("remount,rw" in c for c in calls) and any("remount,ro" in c for c in calls), calls


def test_the_live_apply_on_a_box_already_in_place_touches_nothing(tmp_path):
    box, target, r = _apply(tmp_path, present=True)
    assert r.returncode == 0, r.stdout + r.stderr
    calls = log(box)
    assert not any("remount" in c or "networkctl reload" in c for c in calls), calls


def test_verify_device_bb_runs_the_lib_and_fails_every_bad_facet(tmp_path):
    s = _read(VERIFY)
    start = s.index("\n# (bb) ")
    block = s[start:s.index("\n# (", start + 5)]
    assert "bkshading_camera_link_gather_snippet" in block and 'warn "' not in block
    head = s[:s.index("\n# (", s.index("Checks"))]
    assert "(bb) the camera's USB-C link" in head and "(bb) the camera's USB-C link (issue 1452)" in s
    sha = _bash('. "$1"; bkshading_camera_link_sha', LINK).stdout.strip()
    for gathered, want in ((("LINK_NET_SHA=%s\nLINK_NETWORKD=enabled\nLINK_ENX_DEFAULT=0\nLINK_ENX_DNS=0" % sha), "OK"),
                           ("LINK_NET_SHA=\nLINK_NETWORKD=enabled\nLINK_ENX_DEFAULT=0\nLINK_ENX_DNS=0", "FAIL")):
        script = ('ok() { echo "OK $*"; }; fail() { echo "FAIL $*"; }\n'
                  'ssh_box() { printf "%%s\\n" "$G"; }\n. "%s"\n%s\n' % (LINK, block))
        r = subprocess.run(["bash", "-c", "set -euo pipefail\n" + script], capture_output=True, text=True,
                           env=dict(os.environ, G=gathered))
        assert r.returncode == 0 and r.stdout.startswith(want), (want, r.stdout, r.stderr)


def test_setup_device_and_the_sbc_install_the_link_file():
    setup = _read(SETUP)
    i = setup.index("if ! bkshading_camera_link_install; then")
    assert setup.index("[bkshading-relay]") < i < setup.index("STEP 18: Configure read-only"), "inside the rw window"
    sbc = _read(SBC)
    assert 'BKSHADING_CAMERA_LINK_SYSTEMCTL="$SYSTEMCTL" bkshading_camera_link_install' in sbc
    assert 'BKSHADING_CAMERA_LINK_PATH="$NETWORKD_DIR/05-bkshading-camera.network"' in sbc
    assert "(2c) issue 1452" in sbc


# ---- 3. the preflight's focus line from a REST relay -------------------------------------------- #

@pytest.mark.parametrize("body,want", [
    ('{"params":{"focusNorm":0.4213,"focusDistance":null}}', "0.421"),
    ('{"params":{"focusNorm":0}}', "0"),
    ('{"params":{"focusNorm":true}}', ""),
    ('{"params":{"focusNorm":1.5}}', ""),
    ('{"params":{}}', ""),
    ("garbage", ""),
])
def test_the_preflight_reads_the_rest_relays_focus_norm(body, want):
    r = _bash('. "$1"; bkshading_preflight_state_focus_norm "$2"', PREFLIGHT, body)
    assert r.returncode == 0 and r.stdout.strip() == want, (body, r.stdout, r.stderr)
    m = _bash('. "$1"; bkshading_preflight_focus_norm_message cam3 10.0.0.3 "Pocket 6K" 0.42', PREFLIGHT).stdout
    assert "focusNorm=0.42" in m and "WARNING" not in m and "does NOT prove manual-focus MODE" in m
