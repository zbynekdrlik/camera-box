#!/usr/bin/env python3
"""issue 1371 -- the E2E [0/8] test-camera shutter/ISO enforce over the bkshading USB path.

Owner, 25.9.2026: the test camera's shutter and ISO were wrong, so the test must check AND set
them itself. The step is a pure Python decision module (`scripts/camera_test_settings.py`) + a thin
sourced bash transport (`scripts/lib/camera-test-settings.sh`) + a checked-in baseline
(`scripts/camera-test-baseline.json`) + one call in `scripts/recording-e2e.sh`.

Tier-0 (no cargo, no camera, no rig): the pure module is called directly; the bash lib is driven
end-to-end with a fake `sshpass` (emulating the cambox sysfs + gphoto2) and a fake `obs_phase2.py`
(the issue-1271 rig-busy guard), under the caller's real `set -euo pipefail`.
Runnable directly or under pytest (the `python-tests` CI job).
"""
import importlib.util
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
MODULE = os.path.join(REPO, "scripts", "camera_test_settings.py")
LIB = os.path.join(REPO, "scripts", "lib", "camera-test-settings.sh")
BASELINE = os.path.join(REPO, "scripts", "camera-test-baseline.json")
E2E = os.path.join(REPO, "scripts", "recording-e2e.sh")
TRANSPORT_RS = os.path.join(REPO, "bkshading", "relay", "src", "transport.rs")
READ_RS = os.path.join(REPO, "bkshading", "proto", "src", "read.rs")

_spec = importlib.util.spec_from_file_location("camera_test_settings", MODULE)
cts = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cts)


def _baseline_text(values):
    return json.dumps({"schema": 1, "values": values})


def _all_null():
    return {k: None for k in cts.ENFORCEABLE_KEYS}


def _pinned(**extra):
    v = _all_null()
    v.update({"iso": 400, "d002": "18000"})
    v.update(extra)
    return v


def _blocks(values):
    """gphoto2 multi --get-config stdout for READ_KEYS (a None value = a block with no Current:)."""
    out = []
    for k in cts.READ_KEYS:
        out.append("Label: %s" % k)
        out.append("Type: RANGE")
        if values.get(k) is not None:
            out.append("Current: %s" % values[k])
        out.append("END")
    return "\n".join(out) + "\n"


# ---------------------------------------------------------------------------------------------
# key names come from the relay's own transport source -- never guessed
# ---------------------------------------------------------------------------------------------
def test_every_read_key_is_one_of_the_relay_core_config_keys():
    src = open(TRANSPORT_RS, encoding="utf-8").read()
    m = re.search(r"pub const CORE_CONFIG_KEYS: \[&str; \d+\] = \[([^\]]*)\]", src)
    assert m, "CORE_CONFIG_KEYS not found in bkshading/relay/src/transport.rs"
    relay_keys = re.findall(r'"([^"]+)"', m.group(1))
    for k in cts.READ_KEYS:
        assert k in relay_keys, "key %r is not one of the relay's CORE_CONFIG_KEYS %s" % (k, relay_keys)


def test_every_enforceable_key_is_one_the_relay_writes():
    src = open(READ_RS, encoding="utf-8").read()
    body = src[src.index("pub fn plan_writes("):]
    written = set(re.findall(r'out\.push\(\(\s*"([^"]+)"\.to_string\(\)', body))
    for k in cts.ENFORCEABLE_KEYS:
        assert k in written, "enforceable key %r is not a key plan_writes sets (%s)" % (k, sorted(written))


def test_shutter_and_iso_are_the_required_keys_and_fps_is_never_enforced():
    assert cts.REQUIRED_KEYS == ("iso", "d002")
    assert "d007" not in cts.ENFORCEABLE_KEYS
    assert "d007" in cts.READ_KEYS


# ---------------------------------------------------------------------------------------------
# baseline
# ---------------------------------------------------------------------------------------------
def test_checked_in_baseline_is_valid_and_carries_every_key():
    b = cts.load_baseline(open(BASELINE, encoding="utf-8").read())
    assert set(b) == set(cts.ENFORCEABLE_KEYS)


def test_baseline_values_normalize_to_gphoto2_strings():
    b = cts.load_baseline(_baseline_text(_pinned(**{"f-number": "f/5.6", "d005": -3})))
    assert b["iso"] == "400" and b["d002"] == "18000" and b["f-number"] == "f/5.6" and b["d005"] == "-3"
    assert cts.baseline_pinned(b)
    assert cts.pinned_keys(b) == ["iso", "d002", "f-number", "d005"]


def test_null_required_key_is_unpinned():
    assert not cts.baseline_pinned(cts.load_baseline(_baseline_text(_all_null())))
    half = _all_null()
    half["iso"] = 400
    assert not cts.baseline_pinned(cts.load_baseline(_baseline_text(half)))


def test_malformed_baselines_are_refused():
    bad = [
        "not json",
        "[]",
        json.dumps({"schema": 2, "values": _all_null()}),
        json.dumps({"schema": 1}),
        _baseline_text(dict(_all_null(), shutter=500)),  # unknown key
        _baseline_text({k: None for k in cts.ENFORCEABLE_KEYS if k != "d005"}),  # missing key
        _baseline_text(_pinned(iso=True)),
        _baseline_text(_pinned(iso="")),
        _baseline_text(_pinned(d002="18000; reboot")),
        _baseline_text(_pinned(d002=18000.5)),
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
def test_parse_current_mirrors_the_relay_and_drops_null():
    assert cts.parse_current("Label: ISO\nCurrent: 400\nChoice: 0 100") == "400"
    assert cts.parse_current("Label: F\nCurrent: (null)") is None
    assert cts.parse_current("Label: F\nCurrent:") is None
    assert cts.parse_current("Label: F") is None


def test_split_config_blocks_needs_the_exact_block_count():
    assert cts.split_config_blocks("A\nEND\nB\nEND\n", 2) == ["A", "B"]
    assert cts.split_config_blocks("A\nEND\nB\n", 2) is None
    assert cts.split_config_blocks("A\nEND\nB\nEND\nC\nEND", 2) is None


def test_parse_read_requires_iso_and_shutter_values():
    cur = cts.parse_read(_blocks({"iso": 800, "d002": 9000, "d007": 60}))
    assert cur["iso"] == "800" and cur["d002"] == "9000" and cur["f-number"] is None
    assert cts.parse_read(_blocks({"iso": 800, "d007": 60})) is None
    assert cts.parse_read("") is None


def test_plan_sets_only_the_pinned_keys_that_differ():
    b = cts.load_baseline(_baseline_text(_pinned(d004=5600)))
    cur = {"iso": "800", "d002": "18000", "f-number": "f/2.8", "d004": "5600", "d005": "0", "d007": "60"}
    assert cts.plan_sets(cur, b) == [("iso", "400")]
    cur["d004"] = "3200"
    assert cts.plan_sets(cur, b) == [("iso", "400"), ("d004", "5600")]
    assert cts.plan_sets(dict(cur, iso="400", d004="5600"), b) == []


def test_grade_readback_names_every_pinned_key_that_did_not_apply():
    b = cts.load_baseline(_baseline_text(_pinned()))
    assert cts.grade_readback({"iso": "400", "d002": "18000"}, b) == []
    assert cts.grade_readback({"iso": "400", "d002": "9000"}, b) == [("d002", "18000", "9000")]


def test_shutter_denominator_uses_the_relay_formula():
    assert cts.shutter_denominator("18000", "60") == 120  # 180 deg at 60 fps = 1/120
    assert cts.shutter_denominator("4320", "60") == 500
    assert cts.shutter_denominator(None, "60") is None
    assert cts.shutter_denominator("0", "60") is None


def test_read_args_is_one_session_over_every_read_key():
    args = cts.read_args()
    assert args.count("--get-config") == len(cts.READ_KEYS)
    assert [args[i + 1] for i in range(0, len(args), 2)] == list(cts.READ_KEYS)


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
        r = _cli(["plan", "--baseline", path], _blocks({"iso": 800, "d002": 18000, "d007": 60}))
        assert r.returncode == 0
        assert "BEFORE iso 800" in r.stdout and "SHUTTER BEFORE 1/120 s at 60 fps" in r.stdout
        assert "SET iso 800 -> 400" in r.stdout
        assert "SETARGS --set-config iso=400" in r.stdout
        assert "d002=" not in r.stdout.split("SETARGS", 1)[1]
        r = _cli(["grade", "--baseline", path], _blocks({"iso": 400, "d002": 9000, "d007": 60}))
        assert r.returncode == cts.EXIT_MISMATCH and "MISMATCH d002 want=18000 got=9000" in r.stdout
        assert _cli(["grade", "--baseline", path], _blocks({"iso": 400, "d002": 18000})).returncode == 0
        assert _cli(["plan", "--baseline", path], "garbage").returncode == cts.EXIT_UNREADABLE
    finally:
        os.unlink(path)
    path = _tmp_baseline(dict(_all_null(), shutter=1))
    try:
        assert _cli(["status", "--baseline", path]).returncode == cts.EXIT_BAD_BASELINE
    finally:
        os.unlink(path)


def test_cli_suggest_pins_only_shutter_and_iso():
    r = _cli(["suggest"], _blocks({"iso": 320, "d002": 4320, "f-number": "f/4", "d007": 60}))
    doc = json.loads(r.stdout)
    assert doc["values"]["iso"] == "320" and doc["values"]["d002"] == "4320"
    assert doc["values"]["f-number"] is None
    # the suggestion is itself a valid, pinned baseline
    assert cts.baseline_pinned(cts.load_baseline(r.stdout))


# ---------------------------------------------------------------------------------------------
# the bash transport, end to end with fakes
# ---------------------------------------------------------------------------------------------
FAKE_SSHPASS = r'''#!/usr/bin/env python3
import json, os, re, sys
d = os.environ["FAKE_CAM_DIR"]
args = sys.argv[1:]
ip = next(a[len("root@"):] for a in args if a.startswith("root@"))
cmd = args[-1]
state_path = os.path.join(d, "state.json")
state = json.load(open(state_path))
def log(line):
    with open(os.path.join(d, "calls.log"), "a") as f:
        f.write(line + "\n")
if "CTS_USB" in cmd:
    log("PRESENCE " + ip)
    p = state["present"].get(ip)
    if p is None:
        sys.exit(255)  # unreachable box: no marker line
    print("CTS_USB:%d" % p)
    sys.exit(0)
if "--set-config" in cmd:
    log("SET " + ip + " " + " ".join(re.findall(r"--set-config (\S+)", cmd)))
    for kv in re.findall(r"--set-config (\S+)", cmd):
        k, v = kv.split("=", 1)
        if k not in state.get("ignore", []):
            state["camera"][k] = v
    json.dump(state, open(state_path, "w"))
    sys.exit(0)
if "--get-config" in cmd:
    log("GET " + ip)
    if state.get("read_fail"):
        sys.stderr.write("*** Error: Could not detect any camera\n")
        sys.exit(1)
    for k in re.findall(r"--get-config (\S+)", cmd):
        print("Label: %s" % k)
        if state["camera"].get(k) is not None:
            print("Current: %s" % state["camera"][k])
        print("END")
    sys.exit(0)
sys.exit(99)
'''

FAKE_OBS_PHASE2 = r'''#!/usr/bin/env python3
import json, os, sys
with open(os.path.join(os.environ["FAKE_CAM_DIR"], "calls.log"), "a") as f:
    f.write("GUARD " + sys.argv[1] + "\n")
busy = os.environ.get("FAKE_BUSY") == "1"
print(json.dumps({"busy": busy, "diagnostics": [{"host": "stream", "streaming": busy, "recording": False}]}))
'''


class Rig:
    def __init__(self, present, camera, baseline_values, ack="", busy=False, ignore=(), read_fail=False):
        self.root = tempfile.mkdtemp(prefix="cts1371-")
        self.here = os.path.join(self.root, "scripts")
        self.bin = os.path.join(self.root, "bin")
        os.makedirs(os.path.join(self.here, "lib"))
        os.makedirs(self.bin)
        shutil.copy(MODULE, os.path.join(self.here, "camera_test_settings.py"))
        for name in ("camera-test-settings.sh", "cambox-offline-ack.sh", "stray-session-check.sh"):
            shutil.copy(os.path.join(REPO, "scripts", "lib", name), os.path.join(self.here, "lib", name))
        self._exe(os.path.join(self.bin, "sshpass"), FAKE_SSHPASS)
        self._exe(os.path.join(self.here, "obs_phase2.py"), FAKE_OBS_PHASE2)
        with open(os.path.join(self.here, "camera-test-baseline.json"), "w") as f:
            f.write(_baseline_text(baseline_values))
        with open(os.path.join(self.root, "state.json"), "w") as f:
            json.dump({"present": present, "camera": camera, "ignore": list(ignore), "read_fail": read_fail}, f)
        open(os.path.join(self.root, "calls.log"), "w").close()
        self.ack = ack
        self.busy = busy

    @staticmethod
    def _exe(path, text):
        with open(path, "w") as f:
            f.write(text)
        os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    def run(self):
        script = os.path.join(self.root, "run.sh")
        with open(script, "w") as f:
            f.write(
                "set -euo pipefail\n"
                '. "%s/lib/camera-test-settings.sh"\n'
                'camera_test_settings_enforce "%s" 10.0.0.2 10.0.0.4 pw "cam1=10.77.9.61" "cam2=10.77.9.62"\n'
                'echo "AFTER-ENFORCE rc=0"\n' % (self.here, self.here)
            )
        env = dict(os.environ)
        env["PATH"] = self.bin + os.pathsep + env["PATH"]
        env["FAKE_CAM_DIR"] = self.root
        env["FAKE_BUSY"] = "1" if self.busy else "0"
        env["CAMBOX_OFFLINE_ACK"] = self.ack
        env.pop("CAMERA_TEST_BASELINE", None)
        env.pop("OBS_PASSWORD", None)
        r = subprocess.run(["bash", script], capture_output=True, text=True, env=env)
        self.out = r.stdout + r.stderr
        self.rc = r.returncode
        self.calls = open(os.path.join(self.root, "calls.log")).read().splitlines()
        self.camera = json.load(open(os.path.join(self.root, "state.json")))["camera"]
        shutil.rmtree(self.root)
        return self


GOOD = {"iso": "400", "d002": "18000", "f-number": "f/5.6", "d004": "5600", "d005": "0", "d007": "60"}


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
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="800"), _all_null()).run()
    assert r.rc == 1, r.out
    assert "not pinned" in r.out
    assert '"iso": "800"' in r.out and '"d002": "18000"' in r.out
    assert not any(c.startswith(("SET", "GUARD")) for c in r.calls), r.calls


def test_camera_already_at_baseline_sets_nothing_and_needs_no_guard():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned()).run()
    assert r.rc == 0, r.out
    assert "already at the baseline" in r.out
    assert [c for c in r.calls if c.startswith("GET")] == ["GET 10.77.9.61"]
    assert not any(c.startswith(("SET", "GUARD")) for c in r.calls), r.calls


def test_wrong_shutter_and_iso_are_set_after_the_guard_and_read_back():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600", d002="36000"), _pinned()).run()
    assert r.rc == 0, r.out
    assert "set to the baseline and read back" in r.out
    assert "BEFORE iso 1600" in r.out and "AFTER iso 400" in r.out
    assert "SHUTTER BEFORE 1/60 s" in r.out and "SHUTTER AFTER 1/120 s" in r.out
    seq = [c.split(" ")[0] for c in r.calls if not c.startswith("PRESENCE")]
    assert seq == ["GET", "GUARD", "SET", "GET"], r.calls
    assert "SET 10.77.9.61 iso=400 d002=18000" in r.calls
    assert r.camera["iso"] == "400" and r.camera["d002"] == "18000"


def test_camera_on_cam2_is_resolved_there():
    r = Rig({"10.77.9.61": 0, "10.77.9.62": 1}, dict(GOOD, iso="200"), _pinned()).run()
    assert r.rc == 0, r.out
    assert "cam2 (10.77.9.62)" in r.out
    assert "SET 10.77.9.62 iso=400" in r.calls


def test_a_set_that_does_not_read_back_aborts():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, d002="36000"), _pinned(), ignore=("d002",)).run()
    assert r.rc == 1, r.out
    assert "MISMATCH d002 want=18000 got=36000" in r.out
    assert "did NOT read back" in r.out


def test_gphoto2_read_failure_on_a_present_camera_aborts():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), read_fail=True).run()
    assert r.rc == 1, r.out
    assert "gphoto2 read failed" in r.out


def test_a_live_broadcast_blocks_the_set():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), busy=True).run()
    assert r.rc == 1, r.out
    assert any(c.startswith("GUARD") for c in r.calls)
    assert not any(c.startswith("SET") for c in r.calls), r.calls


def test_invalid_checked_in_baseline_aborts():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), dict(_all_null(), bogus=1)).run()
    assert r.rc == 1, r.out
    assert "invalid" in r.out


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
    lines = [l for l in s.splitlines() if l.startswith("camera_test_settings_enforce ")]
    assert lines == [call], "the call must be ONE bare statement at column 0: %r" % lines
    pause = s.index('bkshading_e2e_pause_stop cam2 "$PAINTER_IP"')
    trap_close = s.index("' EXIT HUP INT TERM", pause)
    at = s.index(call)
    reach = s.index('echo "[0/8] reachability preflight')
    assert pause < trap_close < s.index(src) < at < reach


if __name__ == "__main__":
    import sys

    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
    print("%d tests passed" % len(fns))
    sys.exit(0)
