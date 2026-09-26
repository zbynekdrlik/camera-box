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
import sys
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
# Emulates `sshpass -p PW ssh ... root@IP CMD`. The sysfs presence probe is emulated (the box's
# /sys cannot be faked); every OTHER remote command -- the gphoto2 sessions with their on-box
# single-gphoto2-user checks -- is really RUN by bash, with PATH = only the stub dir (systemctl,
# pgrep, gphoto2 stubs + the real timeout), so the lib's generated remote text is what is tested.
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

STUB_PGREP = r'''
import json, os, sys
d = os.environ["FAKE_CAM_DIR"]
state = json.load(open(os.path.join(d, "state.json")))
assert sys.argv[1:] == ["-x", "gphoto2"], sys.argv
sys.exit(0 if state.get("gphoto2_busy") else 1)
'''

STUB_GPHOTO2 = r'''
import json, os, sys
d = os.environ["FAKE_CAM_DIR"]
ip = os.environ["FAKE_IP"]
state_path = os.path.join(d, "state.json")
state = json.load(open(state_path))
args = sys.argv[1:]
def log(line):
    with open(os.path.join(d, "calls.log"), "a") as f:
        f.write(line + "\n")
if state.get("read_exit"):
    sys.exit(state["read_exit"])
sets = [args[i + 1] for i in range(len(args) - 1) if args[i] == "--set-config"]
gets = [args[i + 1] for i in range(len(args) - 1) if args[i] == "--get-config"]
if sets:
    log("SET " + ip + " " + " ".join(sets))
    # was the production-exposure snapshot already on disk when the camera was changed?
    state["snapshot_at_set"] = os.path.exists(os.path.join(d, "home", ".camera-box", "camera-prod-exposure.json"))
    for kv in sets:
        k, v = kv.split("=", 1)
        if k not in state.get("ignore", []):
            state["camera"][k] = v
    json.dump(state, open(state_path, "w"))
    sys.exit(0)
log("GET " + ip)
state["reads"] = state.get("reads", 0) + 1
json.dump(state, open(state_path, "w"))
if state.get("read_fail"):
    sys.stderr.write("*** Error: Could not detect any camera\n")
    sys.exit(1)
for k in gets:
    print("Label: %s" % k)
    if state["camera"].get(k) is not None:
        print("Current: %s" % state["camera"][k])
    print("END")
sys.exit(0)
'''

FAKE_OBS_PHASE2 = r'''#!/usr/bin/env python3
import json, os, sys
with open(os.path.join(os.environ["FAKE_CAM_DIR"], "calls.log"), "a") as f:
    pw = sys.argv[sys.argv.index("--password") + 1] if "--password" in sys.argv else "-"
    f.write("GUARD " + sys.argv[1] + " pw=" + pw + "\n")
busy = os.environ.get("FAKE_BUSY") == "1"
print(json.dumps({"busy": busy, "diagnostics": [{"host": "stream", "streaming": busy, "recording": False}]}))
'''


class Rig:
    def __init__(self, present, camera, baseline_values, ack="", busy=False, ignore=(), read_fail=False,
                 relay_state="inactive", relay_active_after_reads=None, gphoto2_busy=False, read_exit=0,
                 no_pgrep=False, snapshot=None, pipefail=True, readonly_snap_dir=False, extra_env=None):
        self.root = tempfile.mkdtemp(prefix="cts1371-")
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
                     "bkshading-relay-runtime.sh"):
            shutil.copy(os.path.join(REPO, "scripts", "lib", name), os.path.join(self.here, "lib", name))
        self._exe(os.path.join(self.bin, "sshpass"), FAKE_SSHPASS)
        self._exe(os.path.join(self.here, "obs_phase2.py"), FAKE_OBS_PHASE2)
        # The on-box stubs run with PATH = the stub dir only, so they carry an absolute interpreter.
        shebang = "#!%s\n" % sys.executable
        self._exe(os.path.join(stubs, "systemctl"), shebang + STUB_SYSTEMCTL)
        self._exe(os.path.join(stubs, "gphoto2"), shebang + STUB_GPHOTO2)
        if not no_pgrep:
            self._exe(os.path.join(stubs, "pgrep"), shebang + STUB_PGREP)
        os.symlink(shutil.which("timeout"), os.path.join(stubs, "timeout"))
        with open(os.path.join(self.here, "camera-test-baseline.json"), "w") as f:
            f.write(_baseline_text(baseline_values))
        with open(os.path.join(self.root, "state.json"), "w") as f:
            json.dump({"present": present, "camera": camera, "ignore": list(ignore), "read_fail": read_fail,
                       "relay_state": relay_state, "relay_active_after_reads": relay_active_after_reads,
                       "gphoto2_busy": gphoto2_busy, "read_exit": read_exit, "reads": 0}, f)
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


def test_a_relay_still_active_on_the_camera_box_aborts_by_name():
    # review finding: the issue-808 pause is best-effort, so "exactly one gphoto2 user" is checked
    # on the box, in the same remote command, before every gphoto2 session.
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), relay_state="active").run()
    assert r.rc == 1, r.out
    assert "bkshading-relay is still active on cam1 (10.77.9.61)" in r.out
    assert not any(c.startswith(("GET", "SET", "GUARD")) for c in r.calls), r.calls


def test_a_relay_waiting_to_restart_counts_as_active():
    # `systemctl is-active --quiet` is false for activating/deactivating; only a truly stopped
    # unit lets the session run.
    for st in ("activating", "deactivating", "reloading"):
        r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), relay_state=st).run()
        assert r.rc == 1, (st, r.out)
        assert "bkshading-relay is still active" in r.out, (st, r.out)
    for st in ("inactive", "failed", "unknown"):
        r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), relay_state=st).run()
        assert r.rc == 0, (st, r.out)


def test_a_relay_that_comes_back_between_read_and_set_aborts_the_set():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), relay_active_after_reads=1).run()
    assert r.rc == 1, r.out
    assert "bkshading-relay is still active" in r.out and "the set would race" in r.out
    assert [c.split(" ")[0] for c in r.calls if not c.startswith("PRESENCE")] == ["GET", "GUARD"], r.calls


def test_a_box_without_pgrep_refuses_instead_of_skipping_the_check():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), no_pgrep=True).run()
    assert r.rc == 1, r.out
    assert "pgrep is not available on cam1 (10.77.9.61)" in r.out
    assert not any(c.startswith("GET") for c in r.calls), r.calls


def test_a_leftover_gphoto2_process_aborts_by_name():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), gphoto2_busy=True).run()
    assert r.rc == 1, r.out
    assert "another gphoto2 process is running on cam1 (10.77.9.61)" in r.out


def test_a_transport_timeout_is_named_not_reported_as_unreadable_output():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned(), read_exit=124).run()
    assert r.rc == 1, r.out
    assert "transport rc=124 (timed out)" in r.out


def test_suggest_flags_a_value_that_cannot_be_pinned():
    r = _cli(["suggest"], _blocks({"iso": "Auto ISO", "d002": 4320, "d007": 60}))
    assert r.returncode == 0
    assert "UNPINNABLE iso" in r.stderr


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


# ---------------------------------------------------------------------------------------------
# the production-exposure snapshot + the EVENT-switch restore (owner, 26.9.2026: "ked vypina sa
# development tak ze aj vratis iso a uzavierku naspat"). The E2E snapshots what the camera had right
# before its FIRST set of a development period; rig-mode EVENT puts it back and reads it back.
# ---------------------------------------------------------------------------------------------
RIG_MODE = os.path.join(REPO, "scripts", "rig-mode.sh")
PROD = {"schema": 1, "box": "cam1", "taken_utc": "2026-09-26T15:00:00Z",
        "values": {"iso": "800", "d002": "36000"}, "context": {"d007": "60"}}


def test_default_snapshot_path_is_in_the_runner_home_and_the_env_overrides_it():
    assert cts.default_snapshot_path({"HOME": "/h"}) == "/h/.camera-box/camera-prod-exposure.json"
    assert cts.default_snapshot_path({"HOME": "/h", cts.SNAPSHOT_ENV: "/x/s.json"}) == "/x/s.json"
    assert cts.SNAPSHOT_ENV == "CAMERA_PROD_EXPOSURE_SNAPSHOT"


def test_build_snapshot_records_the_values_the_test_is_about_to_overwrite():
    b = cts.load_baseline(_baseline_text(_pinned()))
    cur = {"iso": "800", "d002": "36000", "f-number": "f/4", "d004": "5600", "d005": "0", "d007": "60"}
    doc = cts.build_snapshot(cur, b, "cam1", "2026-09-26T15:00:00Z")
    assert doc == PROD
    # a pinned optional key is recorded too; an unpinned one is never touched, so never restored
    b = cts.load_baseline(_baseline_text(_pinned(d004=3200)))
    assert cts.build_snapshot(cur, b, "cam1", "t")["values"] == {"iso": "800", "d002": "36000", "d004": "5600"}
    # the round trip is a valid snapshot
    assert cts.load_snapshot(json.dumps(doc)) == doc


def test_build_snapshot_refuses_a_value_it_could_not_restore():
    b = cts.load_baseline(_baseline_text(_pinned()))
    cur = {"iso": "Auto ISO", "d002": "36000", "d007": "60"}
    try:
        cts.build_snapshot(cur, b, "cam1", "t")
    except cts.SnapshotError as e:
        assert "iso" in str(e)
    else:
        raise AssertionError("a value that is not a plain token must not be snapshotted")


def test_malformed_snapshots_are_refused():
    bad = [
        "not json",
        "[]",
        json.dumps(dict(PROD, schema=2)),
        json.dumps({k: v for k, v in PROD.items() if k != "values"}),
        json.dumps(dict(PROD, values={})),
        json.dumps(dict(PROD, values={"d007": "60"})),  # fps is never restored
        json.dumps(dict(PROD, values={"iso": "800; reboot"})),
        json.dumps(dict(PROD, values={"iso": 800})),
        json.dumps(dict(PROD, box="")),
        json.dumps({k: v for k, v in PROD.items() if k != "taken_utc"}),
    ]
    for text in bad:
        try:
            cts.load_snapshot(text)
        except cts.SnapshotError:
            continue
        raise AssertionError("snapshot should be refused: %s" % text)


def test_restore_plan_and_grade_touch_only_the_snapshot_keys_that_differ():
    snap = PROD["values"]
    assert cts.restore_plan(snap, {"iso": "8000", "d002": "2160", "d007": "60"}) == [("iso", "800"), ("d002", "36000")]
    assert cts.restore_plan(snap, {"iso": "800", "d002": "2160"}) == [("d002", "36000")]
    assert cts.restore_plan(snap, {"iso": "800", "d002": "36000"}) == []
    assert cts.grade_restore(snap, {"iso": "800", "d002": "36000"}) == []
    assert cts.grade_restore(snap, {"iso": "800", "d002": "2160"}) == [("d002", "36000", "2160")]


def test_snapshot_summary_is_plain_slovak_with_the_shutter_as_1_over_n():
    assert cts.snapshot_summary(PROD) == "ISO 800, uzávierka 1/60 s (uhol 360°), cam1 2026-09-26T15:00:00Z"
    no_fps = dict(PROD, context={})
    assert cts.snapshot_summary(no_fps) == "ISO 800, uzávierka uhol 360°, cam1 2026-09-26T15:00:00Z"
    odd = dict(PROD, values={"iso": "8000", "d002": "2160"})
    assert "1/1000 s (uhol 21.6°)" in cts.snapshot_summary(odd)


def test_consumed_path_and_the_newest_consumed_snapshot():
    p = "/h/.camera-box/camera-prod-exposure.json"
    assert cts.consumed_path(p, "20260926T160000Z") == "/h/.camera-box/camera-prod-exposure.consumed-20260926T160000Z.json"
    names = ["camera-prod-exposure.consumed-20260920T100000Z.json", "other.json",
             "camera-prod-exposure.consumed-20260926T160000Z.json", "camera-prod-exposure.json"]
    assert cts.newest_consumed(p, names) == "camera-prod-exposure.consumed-20260926T160000Z.json"
    assert cts.newest_consumed(p, ["other.json"]) is None


def test_snapshot_state_line_for_the_handover_check():
    root = tempfile.mkdtemp(prefix="cts1371-state-")
    try:
        p = os.path.join(root, "camera-prod-exposure.json")
        assert cts.snapshot_state(p) == "exposure state=none"
        with open(cts.consumed_path(p, "20260926T160000Z"), "w") as f:
            json.dump(PROD, f)
        line = cts.snapshot_state(p)
        assert line.startswith("exposure state=restored restored=20260926T160000Z box=cam1")
        assert "iso=800 d002=36000" in line
        with open(p, "w") as f:
            json.dump(PROD, f)
        line = cts.snapshot_state(p)
        assert line.startswith("exposure state=pending box=cam1 taken=2026-09-26T15:00:00Z")
        assert "iso=800 d002=36000" in line
        with open(p, "w") as f:
            f.write("{broken")
        assert cts.snapshot_state(p).startswith("exposure state=invalid ")
    finally:
        shutil.rmtree(root)


def test_cli_snapshot_writes_once_and_never_overwrites():
    root = tempfile.mkdtemp(prefix="cts1371-cli-")
    base = _tmp_baseline(_pinned())
    try:
        p = os.path.join(root, "sub", "camera-prod-exposure.json")
        r = _cli(["snapshot", "--baseline", base, "--snapshot", p, "--box", "cam1"],
                 _blocks({"iso": 800, "d002": 36000, "d007": 60}))
        assert r.returncode == 0, r.stderr
        assert "SNAPSHOT saved" in r.stdout
        doc = json.load(open(p))
        assert doc["values"] == {"iso": "800", "d002": "36000"} and doc["box"] == "cam1"
        r = _cli(["snapshot", "--baseline", base, "--snapshot", p, "--box", "cam2"],
                 _blocks({"iso": 8000, "d002": 2160, "d007": 60}))
        assert r.returncode == 0 and "SNAPSHOT kept" in r.stdout
        assert json.load(open(p)) == doc
        # an unrestorable value is refused and nothing is written
        q = os.path.join(root, "q.json")
        r = _cli(["snapshot", "--baseline", base, "--snapshot", q, "--box", "cam1"],
                 _blocks({"iso": "Auto ISO", "d002": 36000, "d007": 60}))
        assert r.returncode == cts.EXIT_SNAPSHOT_FAILED and not os.path.exists(q)
    finally:
        shutil.rmtree(root)
        os.unlink(base)


def test_cli_snapshot_path_honours_the_env_and_home():
    env = dict(os.environ, HOME="/tmp/cts1371-home")
    env.pop(cts.SNAPSHOT_ENV, None)
    r = subprocess.run(["python3", MODULE, "snapshot-path"], capture_output=True, text=True, env=env)
    assert r.stdout.strip() == "/tmp/cts1371-home/.camera-box/camera-prod-exposure.json"
    env[cts.SNAPSHOT_ENV] = "/tmp/x.json"
    r = subprocess.run(["python3", MODULE, "snapshot-path"], capture_output=True, text=True, env=env)
    assert r.stdout.strip() == "/tmp/x.json"


# --- the E2E side: the snapshot is taken before the FIRST set of a development period -----------
def test_the_first_set_snapshots_the_owners_exposure_before_changing_it():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="800", d002="36000"), _pinned()).run()
    assert r.rc == 0, r.out
    doc = json.loads(r.snapshot)
    assert doc["values"] == {"iso": "800", "d002": "36000"}
    assert doc["box"] == "cam1" and doc["context"] == {"d007": "60"}
    assert r.snapshot_at_set is True, "the snapshot must be on disk BEFORE the camera is changed"
    assert "SNAPSHOT saved" in r.out


def test_a_pending_snapshot_is_never_overwritten_by_a_later_run():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), snapshot=PROD).run()
    assert r.rc == 0, r.out
    assert json.loads(r.snapshot) == PROD
    assert "SNAPSHOT kept" in r.out


def test_a_camera_already_at_the_baseline_takes_no_snapshot():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned()).run()
    assert r.rc == 0, r.out
    assert r.snapshot is None


def test_a_set_blocked_by_a_live_broadcast_takes_no_snapshot():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), busy=True).run()
    assert r.rc == 1, r.out
    assert r.snapshot is None


def test_an_unrestorable_camera_value_aborts_before_the_set():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="Auto ISO"), _pinned()).run()
    assert r.rc == 1, r.out
    assert "production exposure" in r.out and "iso" in r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls
    assert r.snapshot is None


def test_an_unwritable_snapshot_aborts_before_the_set():
    # ~/.camera-box is a FILE, so the snapshot cannot be written: the owner's exposure would be lost
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned())
    with open(r.snap_dir, "w") as f:
        f.write("not a dir")
    r.run()
    assert r.rc == 1, r.out
    assert "production exposure" in r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls


# --- the EVENT side: restore, read back, consume ------------------------------------------------
def _restore(camera, snapshot=PROD, **kw):
    return Rig({"10.77.9.61": 1}, camera, _pinned(), snapshot=snapshot, **kw).run(mode="restore")


def test_restore_without_a_snapshot_is_a_quiet_no_op():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned()).run(mode="restore")
    assert r.rc == 0, r.out
    assert "AFTER-RESTORE rc=0 outcome=none" in r.out and "AFTER-NOTE" in r.out
    assert "nothing to restore" in r.out
    assert not any(c.startswith(("PRESENCE", "RELAYSTOP", "GET", "SET", "GUARD")) for c in r.calls), r.calls
    assert r.discord == "EVENT contract ok\n"


def test_restore_puts_the_owners_exposure_back_and_consumes_the_snapshot():
    r = _restore(dict(GOOD, iso="400", d002="18000"))
    assert r.rc == 0, r.out
    assert "AFTER-RESTORE rc=0 outcome=restored" in r.out
    # review round 1: the rig-busy guard runs BEFORE the relay stop and the camera session, and
    # again right before the set (each is a rig mutation during a possibly live broadcast)
    seq = [c.split(" ")[0] for c in r.calls if not c.startswith("PRESENCE")]
    assert seq == ["GUARD", "RELAYSTOP", "GET", "GUARD", "SET", "GET"], r.calls
    assert "SET 10.77.9.61 iso=800 d002=36000" in r.calls
    assert r.camera["iso"] == "800" and r.camera["d002"] == "36000"
    assert r.snapshot is None, "a verified restore moves the snapshot aside"
    assert len(r.consumed) == 1 and r.consumed_docs[0] == PROD
    assert "RESTORE iso 400 -> 800" in r.out and "RESTORED iso 800" in r.out
    assert "✅" in r.discord and "ISO 800" in r.discord and "1/60 s" in r.discord
    assert r.discord.startswith("EVENT contract ok\n"), r.discord


def test_restore_when_the_camera_already_has_the_production_values_sets_nothing():
    r = _restore(dict(GOOD, iso="800", d002="36000"))
    assert r.rc == 0, r.out
    assert "outcome=already" in r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls
    assert [c.split(" ")[0] for c in r.calls if not c.startswith("PRESENCE")] == ["GUARD", "RELAYSTOP", "GET"]
    assert r.snapshot is None and len(r.consumed) == 1


def test_restore_with_the_camera_absent_is_loud_and_keeps_the_snapshot():
    r = Rig({"10.77.9.61": 0, "10.77.9.62": 0}, dict(GOOD), _pinned(), snapshot=PROD).run(mode="restore")
    assert r.rc == 0, r.out  # the EVENT caller itself never aborts
    assert "outcome=failed" in r.out and "AFTER-NOTE" in r.out
    assert "NOT restored" in r.out and "cam1 (10.77.9.61)" in r.out
    assert "::warning" in r.out
    assert json.loads(r.snapshot) == PROD and r.consumed == []
    assert "⚠️" in r.discord and "NEVRÁTILA" in r.discord
    # the failure is ON TOP of the EVENT confirmation, not buried under a green message, and the
    # confirmation itself survives below it
    assert r.discord.startswith("⚠️"), r.discord
    assert "EVENT contract ok" in r.discord
    # setting the camera by hand must also move the snapshot aside, or the next EVENT overwrites it;
    # the command is for the run log -- the owner's phone line asks him to tell Claude instead
    assert "camera_test_settings.py consume" in r.out
    assert "camera_test_settings.py" not in r.discord and "Claud" in r.discord
    # the handover check can tell a failed restore from a snapshot still waiting for its EVENT
    assert r.restore_failed is not None and r.restore_failed["utc"]


def test_restore_that_does_not_read_back_keeps_the_snapshot():
    r = _restore(dict(GOOD, iso="400", d002="18000"), ignore=("d002",))
    assert "outcome=failed" in r.out, r.out
    assert "MISMATCH d002 want=36000 got=18000" in r.out
    assert json.loads(r.snapshot) == PROD and r.consumed == []


def test_restore_blocked_by_a_live_broadcast_keeps_the_snapshot():
    r = _restore(dict(GOOD, iso="400", d002="18000"), busy=True)
    assert "outcome=failed" in r.out, r.out
    assert any(c.startswith("GUARD") for c in r.calls)
    # a live broadcast: no relay stop, no camera session, no set
    assert not any(c.startswith(("RELAYSTOP", "GET", "SET")) for c in r.calls), r.calls
    assert json.loads(r.snapshot) == PROD
    assert "nevysiela" in r.discord


def test_restore_refuses_a_relay_that_comes_back_before_the_set():
    r = _restore(dict(GOOD, iso="400", d002="18000"), relay_active_after_reads=1)
    assert "outcome=failed" in r.out, r.out
    assert "bkshading-relay is still active" in r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls
    assert json.loads(r.snapshot) == PROD


def test_restore_names_a_transport_timeout():
    r = _restore(dict(GOOD), read_exit=124)
    assert "outcome=failed" in r.out, r.out
    assert "transport rc=124 (timed out)" in r.out and "NOT restored" in r.out


def test_restore_of_an_invalid_snapshot_is_loud_and_touches_nothing():
    r = _restore(dict(GOOD), snapshot="{broken")
    assert "outcome=failed" in r.out, r.out
    assert "invalid" in r.out
    assert not any(c.startswith(("PRESENCE", "RELAYSTOP", "GET", "SET")) for c in r.calls), r.calls
    assert r.snapshot == "{broken"


def test_restore_that_cannot_move_the_snapshot_aside_is_restored_but_flagged():
    # review round 1: a verified restore whose consume fails is NOT "not restored"
    r = _restore(dict(GOOD, iso="400", d002="18000"), readonly_snap_dir=True)
    assert "outcome=restored-unconsumed" in r.out, r.out
    assert r.camera["iso"] == "800" and r.camera["d002"] == "36000"
    assert "NOT restored" not in r.out
    assert "camera_test_settings.py consume" in r.out
    assert json.loads(r.snapshot) == PROD
    assert "✅" in r.discord and "odložiť" in r.discord and "EVENT contract ok" in r.discord
    assert "camera_test_settings.py" not in r.discord


def test_a_successful_restore_clears_an_earlier_failure_marker():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="400", d002="18000"), _pinned(), snapshot=PROD)
    with open(os.path.join(r.snap_dir, "camera-prod-exposure.restore-failed.json"), "w") as f:
        json.dump({"utc": "2026-09-26T16:00:00Z", "reason": "camera absent"}, f)
    r.run(mode="restore")
    assert "outcome=restored" in r.out, r.out
    assert r.restore_failed is None


def test_the_snapshot_abort_does_not_depend_on_the_callers_pipefail():
    # review round 1: `python3 ... | prefix || rc=$?` only saw the python rc under pipefail
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), pipefail=False)
    with open(r.snap_dir, "w") as f:
        f.write("not a dir")
    r.run()
    assert r.rc == 1, r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls


def test_build_snapshot_refuses_a_box_or_time_it_could_not_read_back():
    b = cts.load_baseline(_baseline_text(_pinned()))
    cur = {"iso": "800", "d002": "36000", "d007": "60"}
    for box, utc in (("cam 1", "t"), ("cam1", ""), ("cam1;x", "t")):
        try:
            cts.build_snapshot(cur, b, box, utc)
        except cts.SnapshotError:
            continue
        raise AssertionError("box %r / time %r must be refused" % (box, utc))


def test_a_consumed_name_never_overwrites_an_earlier_one():
    p = "/h/.camera-box/camera-prod-exposure.json"
    first = cts.consumed_path(p, "20260926T160000Z")
    taken = {first}
    second = cts.unique_consumed_path(p, "20260926T160000Z", taken.__contains__)
    assert second != first and second.startswith("/h/.camera-box/camera-prod-exposure.consumed-20260926T160000Z")
    names = [os.path.basename(first), os.path.basename(second)]
    # review round 2: the later same-second name is the newest (a text max picked the earlier one)
    assert cts.newest_consumed(p, names) == os.path.basename(second)
    many = ["camera-prod-exposure.consumed-20260926T160000Z-%d.json" % n for n in (2, 10, 9)]
    assert cts.newest_consumed(p, many) == "camera-prod-exposure.consumed-20260926T160000Z-10.json"
    later = "camera-prod-exposure.consumed-20260926T160001Z.json"
    assert cts.newest_consumed(p, many + [later]) == later
    assert cts.unique_consumed_path(p, "20260926T160000Z", lambda _p: False) == first


def test_snapshot_state_reports_a_failed_restore():
    root = tempfile.mkdtemp(prefix="cts1371-failed-")
    try:
        p = os.path.join(root, "camera-prod-exposure.json")
        with open(p, "w") as f:
            json.dump(PROD, f)
        assert "restore_failed=" not in cts.snapshot_state(p)
        env = dict(os.environ, CAMERA_PROD_EXPOSURE_SNAPSHOT=p)
        r = subprocess.run(["python3", MODULE, "restore-failed", "--reason", "camera not on USB"],
                           capture_output=True, text=True, env=env)
        assert r.returncode == 0, r.stderr
        line = cts.snapshot_state(p)
        assert line.startswith("exposure state=pending ") and " restore_failed=" in line
        # consume (the verified restore, or the manual move-aside) clears the failure marker
        r = subprocess.run(["python3", MODULE, "consume"], capture_output=True, text=True, env=env)
        assert r.returncode == 0, r.stderr
        assert not os.path.exists(os.path.join(root, "camera-prod-exposure.restore-failed.json"))
        assert cts.snapshot_state(p).startswith("exposure state=restored ")
    finally:
        shutil.rmtree(root)


def test_the_discord_note_never_fails_without_a_message_file():
    here = os.path.join(REPO, "scripts")
    h = ('. "%s/lib/camera-test-settings.sh"\nCTS_RESTORE_OUTCOME=failed\n'
         'camera_test_settings_restore_discord_note ""\n'
         'camera_test_settings_restore_discord_note /nonexistent/dir/msg.txt\necho NOTE-OK\n' % here)
    r = subprocess.run(["bash", "-c", "set -euo pipefail\n" + h], capture_output=True, text=True)
    assert r.returncode == 0 and "NOTE-OK" in r.stdout, r.stderr


def test_an_invalid_pending_snapshot_is_never_counted_as_kept():
    # review round 2: "No record = no set" -- an unreadable snapshot is no record
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), snapshot="{broken").run()
    assert r.rc == 1, r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls
    assert r.snapshot == "{broken"


def test_a_pending_snapshot_gains_a_newly_pinned_key_but_never_changes_a_kept_one():
    # the baseline pins d004 mid-period: the owner's d004 is still on the camera, so it is added;
    # the kept iso/d002 (the owner's, from before the first set) are never touched
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600", d004="3200"), _pinned(d004=5600), snapshot=PROD).run()
    assert r.rc == 0, r.out
    doc = json.loads(r.snapshot)
    assert doc["values"] == {"iso": "800", "d002": "36000", "d004": "3200"}
    assert doc["box"] == PROD["box"] and doc["taken_utc"] == PROD["taken_utc"]
    assert r.snapshot_at_set is True


def test_a_new_snapshot_drops_a_stale_restore_failed_marker():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned())
    os.makedirs(r.snap_dir)
    with open(os.path.join(r.snap_dir, "camera-prod-exposure.restore-failed.json"), "w") as f:
        json.dump({"utc": "2026-09-20T10:00:00Z", "reason": "old"}, f)
    r.run()
    assert r.rc == 0, r.out
    assert r.snapshot is not None and r.restore_failed is None


def test_the_restore_guard_gets_rig_modes_obs_password():
    # review round 2: rig-mode.sh keeps the OBS WS password in OBS_WS_PASSWORD, the shared guard
    # reads OBS_PASSWORD -- the restore must hand it over, or an auth-enabled OBS fails the guard open
    r = _restore(dict(GOOD, iso="400", d002="18000"), extra_env={"OBS_WS_PASSWORD": "wspw"})
    assert "outcome=restored" in r.out, r.out
    guards = [c for c in r.calls if c.startswith("GUARD")]
    assert guards and all(c.endswith("pw=wspw") for c in guards), guards


def test_a_failed_snapshot_write_leaves_no_temp_file():
    root = tempfile.mkdtemp(prefix="cts1371-tmp-")
    base = _tmp_baseline(_pinned())
    try:
        target = os.path.join(root, "camera-prod-exposure.json")
        orig = cts.os.fsync

        def boom(_fd):
            raise OSError(28, "No space left on device")

        cts.os.fsync = boom
        try:
            doc = cts.build_snapshot({"iso": "800", "d002": "36000"}, cts.load_baseline(open(base).read()),
                                     "cam1", "2026-09-26T15:00:00Z")
            try:
                cts.write_snapshot_once(target, doc)
            except OSError:
                pass
            else:
                raise AssertionError("the failed write must raise")
        finally:
            cts.os.fsync = orig
        assert os.listdir(root) == [], os.listdir(root)
    finally:
        shutil.rmtree(root)
        os.unlink(base)


# --- the rig-mode.sh EVENT wiring (static: #675 sourced helper, never an edited anchor line) ------
def _do_event_body(s):
    start = s.index("\ndo_event() {")
    return s[start:s.index("\nmain() {", start)]


def test_rig_mode_restores_before_the_relay_starts_and_never_aborts_the_event_switch():
    s = open(RIG_MODE, encoding="utf-8").read()
    src = '. "$RIG_MODE_DIR/lib/camera-test-settings.sh"'
    assert s.count(src) == 1
    assert s.index(src) < s.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]; then'), "must be sourced before the source-guard"
    body = _do_event_body(s)
    call = ('camera_test_settings_restore "$RIG_MODE_DIR" "$STRIH_IP" "$STREAM_IP" "$CAM_PW" '
            '"${RIG_SOURCE_BOX}=$RIG_SOURCE_IP" "cam2=$PAINTER_IP" || true')
    assert body.count("camera_test_settings_restore ") == 1
    assert ("\n  " + call + "\n") in body
    # the relay must still be stopped: the restore runs BEFORE the EVENT relay start
    assert body.index(call) < body.index("\n  bkshading_relay_mode_apply event")
    # the outcome reaches the owner's EVENT Discord confirmation, appended before it is sent
    note = 'camera_test_settings_restore_discord_note "${EVENT_ASSERT_DISCORD_MSG_PATH:-}"'
    assert body.count(note) == 1
    assert body.index("\n  event_mode_assert\n") < body.index(note) < body.index("event_mode_discord_confirm_send")


def test_rig_mode_sources_the_restore_helpers():
    h = '. "%s"\ndeclare -F camera_test_settings_restore camera_test_settings_restore_discord_note\n' % RIG_MODE
    r = subprocess.run(["bash", "-c", "set -uo pipefail\n" + h], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    assert "camera_test_settings_restore" in r.stdout


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
    print("%d tests passed" % len(fns))
    sys.exit(0)
