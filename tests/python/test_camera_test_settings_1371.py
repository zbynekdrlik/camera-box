#!/usr/bin/env python3
"""issue 1371 (REST since issue 1452) -- the E2E [0/8] test-camera shutter/ISO enforce.

Owner, 25.9.2026: the test camera's shutter and ISO were wrong, so the test must check AND set
them itself. Since issue 1452 (shading#6, owner 9.11.2026) the step drives the camera over the
Blackmagic REST API on its USB-C link (CDC-NCM), the shading v0.2.x contract's endpoints. The step is
a pure Python decision module (`scripts/camera_test_settings.py`) + a thin sourced bash transport
(`scripts/lib/camera-test-settings.sh`) + a checked-in baseline (`scripts/camera-test-baseline.json`)
+ one call in `scripts/recording-e2e.sh`.

Tier-0 (no cargo, no camera, no rig): the pure module is called directly; the bash lib is driven
end-to-end with a fake `sshpass` that RUNS the generated remote text under bash with PATH = stubs
(`systemctl`, a fake camera `curl`, the real text tools) over a fake /sys/class/net tree + a lease
file, and a fake `obs_phase2.py` (the issue-1271 rig-busy guard), under the caller's real
`set -euo pipefail`. Runnable directly or under pytest (the `python-tests` CI job).
"""
import importlib.util
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
MODULE = os.path.join(REPO, "scripts", "camera_test_settings.py")
LIB = os.path.join(REPO, "scripts", "lib", "camera-test-settings.sh")
BASELINE = os.path.join(REPO, "scripts", "camera-test-baseline.json")
E2E = os.path.join(REPO, "scripts", "recording-e2e.sh")
REST_CONTRACT = os.path.join(REPO, "tests", "fixtures", "shading_pinned_rest", "CONTRACT.md")

_spec = importlib.util.spec_from_file_location("camera_test_settings", MODULE)
cts = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cts)


def _baseline_text(values):
    return json.dumps({"schema": 2, "values": values})


def _all_null():
    return {k: None for k in cts.ENFORCEABLE_KEYS}


def _pinned(**extra):
    v = _all_null()
    v.update({"iso": 400, "shutterAngle": 18000, "shutterSpeed": 120})
    v.update(extra)
    return v


# The camera state the fake curl serves: REST key -> value; `unit` picks how the shutter is reported.
def _read_text(cam, unit="shutterAngle", fps="60", codes=None):
    """The transport's read lines for a camera state (what `camera_test_settings_rest_cmd get` prints)."""
    codes = codes or {}
    bodies = {
        "video/iso": {"iso": cam.get("iso")},
        "video/shutter": {unit: cam.get(unit)},
        "video/whiteBalance": {"whiteBalance": cam.get("whiteBalance")},
        "video/whiteBalanceTint": {"whiteBalanceTint": cam.get("whiteBalanceTint")},
        "lens/iris": {"apertureStop": cam.get("apertureStop"), "normalised": 0.3},
        "system/format": {"frameRate": fps, "codec": "BRaw:Q0"},
    }
    lines = ["CTS_CAMERA enx02aa 172.25.77.161"]
    for ep in cts.READ_ENDPOINTS:
        code = codes.get(ep, "200")
        lines.append("CTS_GET %s %s %s" % (ep, code, json.dumps(bodies[ep]) if code == "200" else ""))
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------------------------
# the endpoints come from the shading REST contract -- never guessed
# ---------------------------------------------------------------------------------------------
def _contract_rows():
    """{endpoint: (json column, relay-use column)} of the REST contract's endpoint table."""
    text = open(REST_CONTRACT, encoding="utf-8").read()
    rows = re.findall(r"^\| `([^`]+)` \| ([^|]*) \| ([^|]*) \| [^|]* \|$", text, re.M)
    assert len(rows) >= 10, rows
    return {ep: (js, use) for ep, js, use in rows}


def test_every_read_endpoint_is_a_contract_endpoint():
    rows = _contract_rows()
    for ep in cts.READ_ENDPOINTS:
        assert ep in rows, "endpoint %r is not in the shading REST contract %s" % (ep, sorted(rows))
        assert "read" in rows[ep][1], (ep, rows[ep])


def test_every_enforceable_field_is_one_the_contract_lets_a_caller_set_on_its_endpoint():
    rows = _contract_rows()
    for key, ep, field in cts.FIELDS:
        js, use = rows[ep]
        assert "set" in use, "the contract does not set %r: %s" % (ep, use)
        assert field in js, "%s is not a field of %s (%s)" % (field, ep, js)


def test_the_aperture_is_set_by_apertureStop_only_and_fps_is_never_enforced():
    # the contract: on lens/iris only apertureStop moves the iris
    assert cts.FIELD_OF["apertureStop"] == "apertureStop"
    assert "system/format" not in cts.ENDPOINT_OF.values()
    assert cts.CONTEXT_ENDPOINT == "system/format" and "system/format" in cts.READ_ENDPOINTS
    assert cts.REQUIRED_ENDPOINTS == ("video/iso", "video/shutter")


# ---------------------------------------------------------------------------------------------
# baseline
# ---------------------------------------------------------------------------------------------
def test_checked_in_baseline_is_valid_pins_both_shutter_units_of_one_exposure():
    b = cts.load_baseline(open(BASELINE, encoding="utf-8").read())
    assert set(b) == set(cts.ENFORCEABLE_KEYS)
    assert cts.baseline_pinned(b)
    # 1/1000 s at 60 fps is an angle of 21.6 deg: both units hold the same owner-confirmed exposure
    assert cts.shutter_denominator("shutterAngle", b["shutterAngle"], "60") == b["shutterSpeed"]


def test_baseline_values_are_numbers():
    b = cts.load_baseline(_baseline_text(_pinned(apertureStop=5.6, whiteBalanceTint=-3)))
    assert b["iso"] == 400 and b["shutterAngle"] == 18000 and b["apertureStop"] == 5.6
    assert b["whiteBalanceTint"] == -3
    assert cts.baseline_pinned(b)


def test_iso_and_one_shutter_unit_make_a_pinned_baseline():
    assert not cts.baseline_pinned(cts.load_baseline(_baseline_text(_all_null())))
    only_iso = dict(_all_null(), iso=400)
    assert not cts.baseline_pinned(cts.load_baseline(_baseline_text(only_iso)))
    assert cts.baseline_pinned(cts.load_baseline(_baseline_text(dict(only_iso, shutterSpeed=500))))
    assert cts.baseline_pinned(cts.load_baseline(_baseline_text(dict(only_iso, shutterAngle=18000))))


def test_malformed_baselines_are_refused():
    bad = [
        "not json",
        "[]",
        json.dumps({"schema": 1, "values": _all_null()}),
        json.dumps({"schema": 2}),
        _baseline_text(dict(_all_null(), d002=500)),  # a gphoto2 key
        _baseline_text({k: None for k in cts.ENFORCEABLE_KEYS if k != "apertureStop"}),  # missing key
        _baseline_text(_pinned(iso=True)),
        _baseline_text(_pinned(iso="400")),
        _baseline_text(_pinned(shutterAngle=18000.5)),
        _baseline_text(_pinned(iso=0)),
        _baseline_text(_pinned(whiteBalance=99999)),
    ]
    for text in bad:
        try:
            cts.load_baseline(text)
        except cts.BaselineError:
            continue
        raise AssertionError("baseline should be refused: %s" % text)


# ---------------------------------------------------------------------------------------------
# parse / plan / grade
# ---------------------------------------------------------------------------------------------
def test_parse_read_takes_the_camera_values_and_its_shutter_unit():
    cur = cts.parse_read(_read_text({"iso": 800, "shutterAngle": 9000, "whiteBalance": 5600}))
    assert cur["iso"] == 800 and cur["shutterAngle"] == 9000 and cur["shutterSpeed"] is None
    assert cur["fps"] == "60" and cur["apertureStop"] is None
    cur = cts.parse_read(_read_text({"iso": 800, "shutterSpeed": 250}, unit="shutterSpeed"))
    assert cts.camera_shutter_key(cur) == "shutterSpeed" and cur["shutterSpeed"] == 250


def test_an_optional_endpoint_the_firmware_does_not_serve_reads_none():
    # the Pocket 6K at fw 8.6 answers 404/501 on some endpoints (the contract's table)
    cur = cts.parse_read(_read_text({"iso": 800, "shutterAngle": 9000},
                                    codes={"video/whiteBalanceTint": "404", "system/format": "501"}))
    assert cur is not None and cur["whiteBalanceTint"] is None and cur["fps"] is None


def test_parse_read_requires_iso_and_shutter_answers():
    assert cts.parse_read(_read_text({"iso": 800, "shutterAngle": 9000}, codes={"video/iso": "404"})) is None
    assert cts.parse_read(_read_text({"iso": 800, "shutterAngle": 9000}, codes={"video/shutter": "500"})) is None
    assert cts.parse_read(_read_text({"iso": 800, "shutterAngle": None})) is None
    assert cts.parse_read("") is None
    assert cts.parse_read("CTS_GET video/iso 200 <html>not a camera</html>\n") is None


def test_plan_sets_only_the_enforced_keys_that_differ_in_the_cameras_unit():
    b = cts.load_baseline(_baseline_text(_pinned(whiteBalance=5600)))
    cur = {"iso": 800, "shutterAngle": 18000, "shutterSpeed": None, "whiteBalance": 5600,
           "whiteBalanceTint": 0, "apertureStop": 2.8, "fps": "60"}
    assert cts.plan_sets(cur, b) == [("iso", 400)]
    cur["whiteBalance"] = 3200
    assert cts.plan_sets(cur, b) == [("iso", 400), ("whiteBalance", 5600)]
    # a camera reporting speed is set in speed, never in angle
    sp = dict(cur, shutterAngle=None, shutterSpeed=60, iso=400, whiteBalance=5600)
    assert cts.plan_sets(sp, b) == [("shutterSpeed", 120)]
    assert cts.set_requests(cts.plan_sets(sp, b)) == [("video/shutter", '{"shutterSpeed":120}')]


def test_a_camera_in_a_shutter_unit_the_baseline_leaves_null_is_refused_never_converted():
    b = cts.load_baseline(_baseline_text(dict(_all_null(), iso=400, shutterAngle=18000)))
    cur = {"iso": 400, "shutterAngle": None, "shutterSpeed": 120, "fps": "60"}
    try:
        cts.plan_sets(cur, b)
    except cts.ShutterUnitError as e:
        assert "shutterSpeed" in str(e)
    else:
        raise AssertionError("must refuse")


def test_grade_readback_names_every_enforced_key_that_did_not_apply():
    b = cts.load_baseline(_baseline_text(_pinned(apertureStop=5.6)))
    good = {"iso": 400, "shutterAngle": 18000, "apertureStop": 5.6}
    assert cts.grade_readback(good, b) == []
    assert cts.grade_readback(dict(good, apertureStop=5.604), b) == []  # 2-decimal camera report
    assert cts.grade_readback(dict(good, shutterAngle=9000), b) == [("shutterAngle", 18000, 9000)]


def test_shutter_denominator_uses_the_relay_formula():
    assert cts.shutter_denominator("shutterAngle", 18000, "60") == 120
    assert cts.shutter_denominator("shutterAngle", 2160, "60") == 1000
    assert cts.shutter_denominator("shutterAngle", 18000, "59.94") == 120
    assert cts.shutter_denominator("shutterAngle", 18000, None) is None
    assert cts.shutter_denominator("shutterSpeed", 500, None) == 500


def test_set_requests_are_one_numeric_field_json_each():
    reqs = cts.set_requests([("iso", 8000), ("apertureStop", 5.6), ("whiteBalanceTint", -3)])
    assert reqs == [("video/iso", '{"iso":8000}'), ("lens/iris", '{"apertureStop":5.6}'),
                    ("video/whiteBalanceTint", '{"whiteBalanceTint":-3}')]


# ---------------------------------------------------------------------------------------------
# the decision matrix
# ---------------------------------------------------------------------------------------------
def test_decide_matrix():
    assert cts.decide(present=True, acked=False, pinned=True) == cts.ENFORCE
    assert cts.decide(present=True, acked=False, pinned=False) == cts.ABORT_UNPINNED
    assert cts.decide(present=True, acked=True, pinned=True) == cts.ABORT_STALE_ACK
    assert cts.decide(present=True, acked=True, pinned=False) == cts.ABORT_STALE_ACK
    assert cts.decide(present=False, acked=False, pinned=True) == cts.ABORT_ABSENT
    assert cts.decide(present=False, acked=False, pinned=False) == cts.UNVERIFIED_UNPINNED
    assert cts.decide(present=False, acked=True, pinned=True) == cts.UNVERIFIED_ACKED
    assert cts.decide(present=False, acked=True, pinned=False) == cts.UNVERIFIED_ACKED


# ---------------------------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------------------------
def _cli(args, stdin=""):
    return subprocess.run(["python3", MODULE] + args, input=stdin, capture_output=True, text=True)


def _tmp_baseline(values):
    fd, path = tempfile.mkstemp(suffix=".json")
    with os.fdopen(fd, "w") as f:
        f.write(_baseline_text(values))
    return path


def test_cli_status_plan_grade():
    path = _tmp_baseline(_pinned())
    try:
        assert _cli(["status", "--baseline", path]).stdout.strip() == "pinned"
        r = _cli(["plan", "--baseline", path], _read_text({"iso": 800, "shutterAngle": 18000}))
        assert r.returncode == 0, r.stderr
        assert "BEFORE iso 800" in r.stdout and "SHUTTER BEFORE 1/120 s at 60 fps" in r.stdout
        assert "SET iso 800 -> 400" in r.stdout
        assert 'SETREQ video/iso {"iso":400}' in r.stdout
        assert "video/shutter" not in r.stdout.split("SETREQ", 1)[1]
        r = _cli(["grade", "--baseline", path], _read_text({"iso": 400, "shutterAngle": 9000}))
        assert r.returncode == cts.EXIT_MISMATCH and "MISMATCH shutterAngle want=18000 got=9000" in r.stdout
        assert _cli(["grade", "--baseline", path], _read_text({"iso": 400, "shutterAngle": 18000})).returncode == 0
        assert _cli(["plan", "--baseline", path], "garbage").returncode == cts.EXIT_UNREADABLE
    finally:
        os.unlink(path)
    path = _tmp_baseline(dict(_all_null(), iso=400, shutterAngle=18000))
    try:
        r = _cli(["plan", "--baseline", path], _read_text({"iso": 400, "shutterSpeed": 60}, unit="shutterSpeed"))
        assert r.returncode == cts.EXIT_SHUTTER_UNIT and "shutterSpeed" in r.stderr
    finally:
        os.unlink(path)
    path = _tmp_baseline(dict(_all_null(), shutter=1))
    try:
        assert _cli(["status", "--baseline", path]).returncode == cts.EXIT_BAD_BASELINE
    finally:
        os.unlink(path)


def test_cli_suggest_pins_iso_and_the_cameras_shutter_unit():
    r = _cli(["suggest"], _read_text({"iso": 320, "shutterSpeed": 500, "apertureStop": 4.0}, unit="shutterSpeed"))
    doc = json.loads(r.stdout)
    assert doc["values"]["iso"] == 320 and doc["values"]["shutterSpeed"] == 500
    assert doc["values"]["shutterAngle"] is None and doc["values"]["apertureStop"] is None
    assert cts.baseline_pinned(cts.load_baseline(r.stdout))


def test_cli_read_endpoints():
    assert _cli(["read-endpoints"]).stdout.split() == list(cts.READ_ENDPOINTS)


# ---------------------------------------------------------------------------------------------
# the bash transport, end to end with fakes
# ---------------------------------------------------------------------------------------------
FAKE_SSHPASS = r'''#!/usr/bin/env python3
# Emulates `sshpass -p PW ssh ... root@IP CMD`. The sysfs presence probe is emulated; every OTHER
# remote command -- the REST sessions with their on-box checks + camera discovery -- is really RUN by
# bash, with PATH = only the stub dir (systemctl, a fake camera curl, real text tools), so the lib's
# generated remote text is what is tested.
import json, os, subprocess, sys
d = os.environ["FAKE_CAM_DIR"]
args = sys.argv[1:]
ip = next(a[len("root@"):] for a in args if a.startswith("root@"))
cmd = args[-1]
state = json.load(open(os.path.join(d, "state.json")))
if "CTS_USB" in cmd:
    with open(os.path.join(d, "calls.log"), "a") as f:
        f.write("PRESENCE " + ip + "\n")
    p = state["present"].get(ip)
    if p is None:
        sys.exit(255)  # unreachable box: no marker line
    print("CTS_USB:%d" % p)
    sys.exit(0)
if state.get("ssh_exit"):
    sys.exit(state["ssh_exit"])
env = {"PATH": os.path.join(d, "stubs"), "FAKE_CAM_DIR": d, "FAKE_IP": ip}
r = subprocess.run(["/bin/bash", "-c", cmd], env=env)
sys.exit(r.returncode)
'''

STUB_SYSTEMCTL = r'''
import json, os, sys
d = os.environ["FAKE_CAM_DIR"]
state = json.load(open(os.path.join(d, "state.json")))
assert sys.argv[2] == "bkshading-relay.service", sys.argv
if sys.argv[1] == "stop":
    # the production-exposure restore stops the relay on the camera box before its read
    with open(os.path.join(d, "calls.log"), "a") as f:
        f.write("RELAYSTOP " + os.environ["FAKE_IP"] + "\n")
    state["relay_state"] = "inactive"
    json.dump(state, open(os.path.join(d, "state.json"), "w"))
    sys.exit(0)
assert sys.argv[1] == "is-active", sys.argv
s = state.get("relay_state", "inactive")
after = state.get("relay_active_after_reads")
if after is not None and state.get("reads", 0) >= after:
    s = "active"
print(s)
sys.exit(0 if s == "active" else 3)
'''

# The camera: GET returns the endpoint body from the state, PUT applies a field unless `ignore` holds
# it (a camera that silently ignores a write). Logs `GET <ip>` once per read session (on video/iso)
# and `SET <ip> <key>=<value>` per PUT.
STUB_CURL = r'''
import json, os, sys
d = os.environ["FAKE_CAM_DIR"]
ip = os.environ["FAKE_IP"]
state_path = os.path.join(d, "state.json")
state = json.load(open(state_path))
a = sys.argv[1:]
def opt(name):
    return a[a.index(name) + 1] if name in a else None
url = [x for x in a if x.startswith("http://")][0]
assert url.startswith("http://172.25.77.161/control/api/v1/"), url
ep = url.split("/control/api/v1/", 1)[1]
def log(line):
    with open(os.path.join(d, "calls.log"), "a") as f:
        f.write(line + "\n")
if state.get("curl_dead"):
    sys.stdout.write("000")
    sys.exit(7)
cam = state["camera"]
unit = state.get("unit", "shutterAngle")
if opt("-X") == "PUT":
    body = json.loads(opt("-d"))
    (field, value), = body.items()
    log("SET %s %s=%s" % (ip, field, value))
    state["snapshot_at_set"] = os.path.exists(os.path.join(d, "home", ".camera-box", "camera-prod-exposure.json"))
    if field not in state.get("ignore", []):
        cam[field] = value
    json.dump(state, open(state_path, "w"))
    sys.stdout.write("204")
    sys.exit(0)
if ep == "video/iso":
    log("GET " + ip)
    state["reads"] = state.get("reads", 0) + 1
    json.dump(state, open(state_path, "w"))
bodies = {
    "video/iso": {"iso": cam.get("iso")},
    "video/shutter": {unit: cam.get(unit)},
    "video/whiteBalance": {"whiteBalance": cam.get("whiteBalance")},
    "video/whiteBalanceTint": {"whiteBalanceTint": cam.get("whiteBalanceTint")},
    "lens/iris": {"apertureStop": cam.get("apertureStop"), "normalised": 0.3},
    "system/format": {"frameRate": cam.get("fps", "60")},
}
code = "404" if ep in state.get("not_served", []) or ep not in bodies else "200"
with open(opt("-o"), "w") as f:
    f.write(json.dumps(bodies[ep], indent=1) if code == "200" else "<html>404</html>")
sys.stdout.write(code)
'''

FAKE_OBS_PHASE2 = r'''#!/usr/bin/env python3
import json, os, sys
with open(os.path.join(os.environ["FAKE_CAM_DIR"], "calls.log"), "a") as f:
    pw = sys.argv[sys.argv.index("--password") + 1] if "--password" in sys.argv else "-"
    f.write("GUARD " + sys.argv[1] + " pw=" + pw + "\n")
busy = os.environ.get("FAKE_BUSY") == "1"
print(json.dumps({"busy": busy, "diagnostics": [{"host": "stream", "streaming": busy, "recording": False}]}))
'''

BOX_TOOLS = ("readlink", "cat", "sed", "head", "cut", "tr", "rm", "awk")


def _make_net(root, link=True, lease=True):
    """A fake /sys/class/net: the LAN NIC (an e1000e PCI device) and, with `link`, the camera's
    cdc_ncm link on a vendor-1edb USB device, ifindex 7, plus its networkd lease."""
    net = os.path.join(root, "net")
    dev = os.path.join(root, "devices")
    leases = os.path.join(root, "leases")
    for p in (net, leases, os.path.join(dev, "drivers", "cdc_ncm"), os.path.join(dev, "drivers", "e1000e")):
        os.makedirs(p, exist_ok=True)
    pci = os.path.join(dev, "pci", "0000:01:00.0")
    os.makedirs(pci)
    os.symlink(os.path.join(dev, "drivers", "e1000e"), os.path.join(pci, "driver"))
    os.makedirs(os.path.join(net, "enp1s0"))
    os.symlink(pci, os.path.join(net, "enp1s0", "device"))
    if link:
        usb = os.path.join(dev, "usb", "4-2")
        intf = os.path.join(usb, "4-2:1.5")
        os.makedirs(intf)
        with open(os.path.join(usb, "idVendor"), "w") as f:
            f.write("1edb\n")
        os.symlink(os.path.join(dev, "drivers", "cdc_ncm"), os.path.join(intf, "driver"))
        n = os.path.join(net, "enx02743ba02a02")
        os.makedirs(n)
        os.symlink(intf, os.path.join(n, "device"))
        with open(os.path.join(n, "ifindex"), "w") as f:
            f.write("7\n")
        if lease:
            with open(os.path.join(leases, "7"), "w") as f:
                f.write("# networkd lease\nADDRESS=172.25.77.162\nSERVER_ADDRESS=172.25.77.161\n")
    return net, leases


class Rig:
    def __init__(self, present, camera, baseline_values, ack="", busy=False, ignore=(), read_fail=False,
                 relay_state="inactive", relay_active_after_reads=None, ssh_exit=0, no_curl=False,
                 link=True, unit="shutterAngle", not_served=(), snapshot=None, pipefail=True,
                 readonly_snap_dir=False, extra_env=None):
        self.root = tempfile.mkdtemp(prefix="cts1452-")
        # A temp HOME: the production-exposure snapshot lives in ~/.camera-box on the runner, and a
        # test must never read or write the real one.
        self.home = os.path.join(self.root, "home")
        self.snap_dir = os.path.join(self.home, ".camera-box")
        self.snap_path = os.path.join(self.snap_dir, "camera-prod-exposure.json")
        os.makedirs(self.home)
        if snapshot is not None:
            os.makedirs(self.snap_dir)
            with open(self.snap_path, "w") as f:
                f.write(snapshot if isinstance(snapshot, str) else json.dumps(snapshot))
        self.shell_opts = "set -euo pipefail\n" if pipefail else "set -eu\n"
        self.readonly_snap_dir = readonly_snap_dir
        self.extra_env = dict(extra_env or {})
        self.here = os.path.join(self.root, "scripts")
        self.bin = os.path.join(self.root, "bin")
        stubs = os.path.join(self.root, "stubs")
        os.makedirs(os.path.join(self.here, "lib"))
        os.makedirs(self.bin)
        os.makedirs(stubs)
        shutil.copy(MODULE, os.path.join(self.here, "camera_test_settings.py"))
        for name in ("camera-test-settings.sh", "cambox-offline-ack.sh", "stray-session-check.sh",
                     "bkshading-relay-runtime.sh", "event-mode-discord-confirm.sh"):
            shutil.copy(os.path.join(REPO, "scripts", "lib", name), os.path.join(self.here, "lib", name))
        self._exe(os.path.join(self.bin, "sshpass"), FAKE_SSHPASS)
        self._exe(os.path.join(self.here, "obs_phase2.py"), FAKE_OBS_PHASE2)
        # The on-box stubs run with PATH = the stub dir only, so they carry an absolute interpreter.
        shebang = "#!%s\n" % sys.executable
        self._exe(os.path.join(stubs, "systemctl"), shebang + STUB_SYSTEMCTL)
        if not no_curl:
            self._exe(os.path.join(stubs, "curl"), shebang + STUB_CURL)
        for tool in BOX_TOOLS:
            os.symlink(shutil.which(tool, path="/usr/bin:/bin"), os.path.join(stubs, tool))
        self.net, self.leases = _make_net(self.root, link=link)
        with open(os.path.join(self.here, "camera-test-baseline.json"), "w") as f:
            f.write(_baseline_text(baseline_values))
        with open(os.path.join(self.root, "state.json"), "w") as f:
            json.dump({"present": present, "camera": camera, "ignore": list(ignore), "curl_dead": read_fail,
                       "relay_state": relay_state, "relay_active_after_reads": relay_active_after_reads,
                       "ssh_exit": ssh_exit, "unit": unit, "not_served": list(not_served), "reads": 0}, f)
        open(os.path.join(self.root, "calls.log"), "w").close()
        self.ack = ack
        self.busy = busy

    @staticmethod
    def _exe(path, text):
        with open(path, "w") as f:
            f.write(text)
        os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    def run(self, mode="enforce"):
        script = os.path.join(self.root, "run.sh")
        with open(script, "w") as f:
            if mode == "enforce":
                f.write(
                    self.shell_opts
                    + '. "%s/lib/camera-test-settings.sh"\n'
                    'camera_test_settings_enforce "%s" 10.0.0.2 10.0.0.4 pw "cam1=10.77.9.61" "cam2=10.77.9.62"\n'
                    'echo "AFTER-ENFORCE rc=0"\n' % (self.here, self.here)
                )
            else:
                # the rig-mode EVENT caller: under set -euo pipefail, the restore must NEVER abort it
                f.write(
                    "set -euo pipefail\n"
                    '. "%s/lib/camera-test-settings.sh"\n'
                    "rrc=0\n"
                    'camera_test_settings_restore "%s" 10.0.0.2 10.0.0.4 pw "cam1=10.77.9.61" "cam2=10.77.9.62" || rrc=$?\n'
                    'echo "AFTER-RESTORE rc=$rrc outcome=${CTS_RESTORE_OUTCOME:-}"\n'
                    "printf 'EVENT contract ok\\n' >\"%s/discord.txt\"\n"
                    'camera_test_settings_restore_discord_note "%s/discord.txt"\n'
                    'echo "AFTER-NOTE"\n' % (self.here, self.here, self.root, self.root)
                )
        env = dict(os.environ)
        env["HOME"] = self.home
        env.pop("CAMERA_PROD_EXPOSURE_SNAPSHOT", None)
        env["PATH"] = self.bin + os.pathsep + env["PATH"]
        env["FAKE_CAM_DIR"] = self.root
        env["FAKE_BUSY"] = "1" if self.busy else "0"
        env["CAMBOX_OFFLINE_ACK"] = self.ack
        env["CAMERA_TEST_SETTINGS_NET_SYSFS"] = self.net
        env["CAMERA_TEST_SETTINGS_LEASES_DIR"] = self.leases
        env.pop("CAMERA_TEST_BASELINE", None)
        env.pop("OBS_PASSWORD", None)
        env.pop("OBS_WS_PASSWORD", None)
        env.update(self.extra_env)
        if self.readonly_snap_dir:
            os.chmod(self.snap_dir, 0o555)
        r = subprocess.run(["bash", script], capture_output=True, text=True, env=env)
        self.out = r.stdout + r.stderr
        self.rc = r.returncode
        self.calls = open(os.path.join(self.root, "calls.log")).read().splitlines()
        if self.readonly_snap_dir:
            os.chmod(self.snap_dir, 0o755)
        state = json.load(open(os.path.join(self.root, "state.json")))
        self.camera = state["camera"]
        self.snapshot_at_set = state.get("snapshot_at_set")
        self.snapshot = None
        if os.path.exists(self.snap_path):
            with open(self.snap_path) as f:
                self.snapshot = f.read()
        self.consumed = sorted(n for n in (os.listdir(self.snap_dir) if os.path.isdir(self.snap_dir) else [])
                               if ".consumed-" in n)
        failed = os.path.join(self.snap_dir, "camera-prod-exposure.restore-failed.json")
        self.restore_failed = json.load(open(failed)) if os.path.exists(failed) else None
        self.consumed_docs = [json.load(open(os.path.join(self.snap_dir, n))) for n in self.consumed]
        dpath = os.path.join(self.root, "discord.txt")
        self.discord = open(dpath).read() if os.path.exists(dpath) else None
        shutil.rmtree(self.root)
        return self


GOOD = {"iso": 400, "shutterAngle": 18000, "whiteBalance": 5600, "whiteBalanceTint": 0, "apertureStop": 5.6,
        "fps": "60"}


def _seq(r):
    return [c.split(" ")[0] for c in r.calls if not c.startswith("PRESENCE")]


def test_lib_parses():
    assert subprocess.run(["bash", "-n", LIB]).returncode == 0


def test_absent_camera_with_null_baseline_is_a_loud_report_only_unverified():
    r = Rig({"10.77.9.61": 0, "10.77.9.62": 0}, {}, _all_null()).run()
    assert r.rc == 0, r.out
    assert "AFTER-ENFORCE" in r.out
    assert "UNVERIFIED" in r.out and "::warning" in r.out
    assert "not on USB" in r.out
    assert not any(c.startswith(("GET", "SET", "GUARD")) for c in r.calls), r.calls


def test_absent_camera_with_pinned_baseline_aborts_by_name():
    r = Rig({"10.77.9.61": 0, "10.77.9.62": 0}, {}, _pinned()).run()
    assert r.rc == 1, r.out
    assert "AFTER-ENFORCE" not in r.out
    assert "NOT on USB" in r.out and "cam1 (10.77.9.61)" in r.out and "testcam" in r.out


def test_unreachable_candidate_is_named_and_still_aborts_when_pinned():
    r = Rig({"10.77.9.62": 0}, {}, _pinned()).run()
    assert r.rc == 1, r.out
    assert "UNREADABLE: cam1 (10.77.9.61)" in r.out


def test_acked_absent_camera_is_a_loud_unverified_even_when_pinned():
    r = Rig({"10.77.9.61": 0, "10.77.9.62": 0}, {}, _pinned(), ack="testcam:usb-unplugged").run()
    assert r.rc == 0, r.out
    assert "EXCLUDED" in r.out and "usb-unplugged" in r.out and "UNVERIFIED" in r.out


def test_acked_but_present_camera_is_a_stale_ack_abort():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), ack="testcam:usb-unplugged").run()
    assert r.rc == 1, r.out
    assert "STALE ACK" in r.out


def test_present_camera_with_null_baseline_refuses_and_prints_the_values_to_pin():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso=800), _all_null()).run()
    assert r.rc == 1, r.out
    assert "not pinned" in r.out
    assert '"iso": 800' in r.out and '"shutterAngle": 18000' in r.out
    assert not any(c.startswith(("SET", "GUARD")) for c in r.calls), r.calls


def test_camera_already_at_baseline_sets_nothing_and_needs_no_guard():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned()).run()
    assert r.rc == 0, r.out
    assert "already at the baseline" in r.out
    assert "CTS_CAMERA enx02743ba02a02 172.25.77.161" in r.out
    assert [c for c in r.calls if c.startswith("GET")] == ["GET 10.77.9.61"]
    assert not any(c.startswith(("SET", "GUARD")) for c in r.calls), r.calls


def test_wrong_shutter_and_iso_are_set_after_the_guard_and_read_back():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso=1600, shutterAngle=36000), _pinned()).run()
    assert r.rc == 0, r.out
    assert "set to the baseline and read back" in r.out
    assert "BEFORE iso 1600" in r.out and "AFTER iso 400" in r.out
    assert "SHUTTER BEFORE 1/60 s" in r.out and "SHUTTER AFTER 1/120 s" in r.out
    assert _seq(r) == ["GET", "GUARD", "SET", "SET", "GET"], r.calls
    assert "SET 10.77.9.61 iso=400" in r.calls and "SET 10.77.9.61 shutterAngle=18000" in r.calls
    assert r.camera["iso"] == 400 and r.camera["shutterAngle"] == 18000


def test_a_camera_that_reports_shutter_speed_is_set_in_speed():
    r = Rig({"10.77.9.61": 1}, {"iso": 400, "shutterSpeed": 60}, _pinned(), unit="shutterSpeed").run()
    assert r.rc == 0, r.out
    assert "SET 10.77.9.61 shutterSpeed=120" in r.calls
    assert not any("shutterAngle" in c for c in r.calls if c.startswith("SET")), r.calls


def test_a_shutter_unit_the_baseline_does_not_pin_aborts_by_name():
    b = dict(_all_null(), iso=400, shutterAngle=18000)
    r = Rig({"10.77.9.61": 1}, {"iso": 400, "shutterSpeed": 60}, b, unit="shutterSpeed").run()
    assert r.rc == 1, r.out
    assert "shutterSpeed" in r.out and "refusing to guess a conversion" in r.out
    assert not any(c.startswith(("SET", "GUARD")) for c in r.calls), r.calls


def test_the_fps_is_only_read_never_written():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso=1600, fps="50"), _pinned()).run()
    assert r.rc == 0, r.out
    assert "SHUTTER BEFORE 1/100 s at 50 fps" in r.out
    assert not any("frameRate" in c or "fps" in c for c in r.calls if c.startswith("SET")), r.calls


def test_camera_on_cam2_is_resolved_there():
    r = Rig({"10.77.9.61": 0, "10.77.9.62": 1}, dict(GOOD, iso=200), _pinned()).run()
    assert r.rc == 0, r.out
    assert "cam2 (10.77.9.62)" in r.out
    assert "SET 10.77.9.62 iso=400" in r.calls


def test_a_set_that_does_not_read_back_aborts():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, shutterAngle=36000), _pinned(), ignore=("shutterAngle",)).run()
    assert r.rc == 1, r.out
    assert "MISMATCH shutterAngle want=18000 got=36000" in r.out
    assert "did NOT read back" in r.out


def test_a_camera_that_does_not_answer_on_its_link_aborts_as_unreadable():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), read_fail=True).run()
    assert r.rc == 1, r.out
    assert "REST read was unreadable" in r.out


def test_a_camera_on_usb_without_a_network_link_aborts_naming_the_link_and_web_media_manager():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), link=False).run()
    assert r.rc == 1, r.out
    assert "has no network link" in r.out and "Web media manager" in r.out and "(bb)" in r.out
    assert not any(c.startswith(("GET", "SET")) for c in r.calls), r.calls


def test_the_camera_address_falls_back_to_the_other_host_of_the_links_30():
    # no lease file: `ip -4 -o addr show dev <link>` (a stub) gives the box .162/30, so the camera is .161
    rig = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned())
    os.unlink(os.path.join(rig.leases, "7"))
    Rig._exe(os.path.join(rig.root, "stubs", "ip"),
             "#!/bin/sh\necho '7: enx02743ba02a02    inet 172.25.77.162/30 brd 172.25.77.163 scope global'\n")
    r = rig.run()
    assert r.rc == 0, r.out
    assert "CTS_CAMERA enx02743ba02a02 172.25.77.161" in r.out


def test_a_relay_still_active_on_the_camera_box_aborts_by_name():
    # the issue-808 pause is best-effort, so the relay is checked on the box before every session
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso=1600), _pinned(), relay_state="active").run()
    assert r.rc == 1, r.out
    assert "bkshading-relay is still active on cam1 (10.77.9.61)" in r.out
    assert not any(c.startswith(("GET", "SET", "GUARD")) for c in r.calls), r.calls


def test_a_relay_waiting_to_restart_counts_as_active():
    for st in ("activating", "deactivating", "reloading"):
        r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), relay_state=st).run()
        assert r.rc == 1, (st, r.out)
        assert "bkshading-relay is still active" in r.out, (st, r.out)
    for st in ("inactive", "failed", "unknown"):
        r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), relay_state=st).run()
        assert r.rc == 0, (st, r.out)


def test_a_relay_that_comes_back_between_read_and_set_aborts_the_set():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso=1600), _pinned(), relay_active_after_reads=1).run()
    assert r.rc == 1, r.out
    assert "bkshading-relay is still active" in r.out and "the set would race" in r.out
    assert _seq(r) == ["GET", "GUARD"], r.calls


def test_a_box_without_curl_refuses_by_name():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), no_curl=True).run()
    assert r.rc == 1, r.out
    assert "curl is not available on cam1 (10.77.9.61)" in r.out
    assert not any(c.startswith("GET") for c in r.calls), r.calls


def test_a_transport_timeout_is_named_not_reported_as_unreadable_output():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), ssh_exit=124).run()
    assert r.rc == 1, r.out
    assert "transport rc=124 (timed out)" in r.out


def test_a_live_broadcast_blocks_the_set():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso=1600), _pinned(), busy=True).run()
    assert r.rc == 1, r.out
    assert any(c.startswith("GUARD") for c in r.calls)
    assert not any(c.startswith("SET") for c in r.calls), r.calls


def test_invalid_checked_in_baseline_aborts():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), dict(_all_null(), bogus=1)).run()
    assert r.rc == 1, r.out
    assert "invalid" in r.out


def test_a_put_body_is_refused_before_it_reaches_the_box_unless_it_is_one_numeric_field():
    def run(spec):
        return subprocess.run(["bash", "-c", '. "$1"; camera_test_settings_rest_cmd put "$2"', "t", LIB, spec],
                              capture_output=True, text=True)
    assert run('video/iso|{"iso":8000}').returncode == 0
    assert run('lens/iris|{"apertureStop":5.6}').returncode == 0
    for bad in ('video/iso|{"iso":"8000; reboot"}', "video/iso|{\"iso\":1}'x", 'system/format|{"frameRate":24}',
                'video/iso|{"iso":8000,"x":1}'):
        r = run(bad)
        assert r.returncode == 1 and "refused PUT" in r.stderr, (bad, r.stdout)


# ---------------------------------------------------------------------------------------------
# the recording-e2e.sh wiring (static: the #675 sourced-helper, one call, placed after the relay
# pause + its temporary restore trap and before the reachability banner)
# ---------------------------------------------------------------------------------------------
def test_recording_e2e_calls_the_step_once_after_the_relay_pause_trap():
    s = open(E2E, encoding="utf-8").read()
    src = '. "$HERE/lib/camera-test-settings.sh"'
    call = 'camera_test_settings_enforce "$HERE" "$STRIH" "$STREAM" "$CAM_PW" "$CAMERA_NAME=$CAM1_IP" "cam2=$PAINTER_IP"'
    assert s.count(src) == 1
    assert s.count("camera_test_settings_enforce ") == 1
    lines = [ln for ln in s.splitlines() if ln.startswith("camera_test_settings_enforce ")]
    assert lines == [call], "the call must be ONE bare statement at column 0: %r" % lines
    pause = s.index('bkshading_e2e_pause_stop cam2 "$PAINTER_IP"')
    trap_close = s.index("' EXIT HUP INT TERM", pause)
    at = s.index(call)
    reach = s.index('echo "[0/8] reachability preflight')
    assert pause < trap_close < s.index(src) < at < reach


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
    print("%d tests passed" % len(fns))
    sys.exit(0)
