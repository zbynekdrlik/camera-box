"""issue 1387 -- the cambox JPEG XS link target, provisioned (scripts/lib/cambox-jxs-target.sh).

The target used to live only in a /run drop-in (`systemctl edit --runtime`): a reboot dropped the camera
to NDI only. Now it is derived from the venue fact files (the venue strih's STRIH_IP + the camera's
STRIH_JXS_CAMERAS port, the ONE camera -> venue-strih lookup of remote-logging.sh), written by
setup-device STEP 7, graded by verify-device (ay), and put on a live box by the apply program inside ONE
verified rw window.

The apply runs on the issue-1407 fake read-only-root box (ro_window_fakes_1407.py) with a systemctl
wrapper that models camera-box's loaded environment (recomputed at daemon-reload from the drop-in files)
and its running environ (taken from the loaded one at a restart). Nothing touches the host.
"""
import hashlib
import os
import pathlib
import re
import shutil
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from ro_window_fakes_1407 import build, index, log, make_box, root, run_text, starts_on_rw  # noqa: E402

REPO = HERE.parents[1]
LIB = REPO / "scripts" / "lib" / "cambox-jxs-target.sh"
FACTS = REPO / "scripts" / "strih-boxes"
SETUP = REPO / "scripts" / "setup-device.sh"
VERIFY = REPO / "scripts" / "verify-device.sh"
JXS_SEND = REPO / "src" / "jxs_send.rs"


def _bash(body, *args, env=None):
    e = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp")}
    e.update(env or {})
    return subprocess.run(["/bin/bash", "-c", f'set -euo pipefail\n. "{LIB}"\n{body}', "harness", *args],
                          env=e, capture_output=True, text=True, timeout=60)


def _fact(box, key):
    for line in (FACTS / f"{box}.env").read_text().splitlines():
        if line.startswith(key + "="):
            return line.split("=", 1)[1]
    raise KeyError(key)


# --- the target, from the venue fact files ----------------------------------------------------------

def test_every_camera_resolves_its_declared_target_or_none():
    for box in ("strih-snv", "strih-pp"):
        ip = _fact(box, "STRIH_IP")
        jxs = dict(e.split(":") for e in _fact(box, "STRIH_JXS_CAMERAS").split()) \
            if _fact(box, "STRIH_JXS_CAMERAS") != "none" else {}
        for n in _fact(box, "STRIH_CAMERAS").split():
            r = _bash('cambox_jxs_target "$1"', f"cam{n}")
            assert r.returncode == 0, r.stderr
            want = f"{ip}:{jxs[n]}" if n in jxs else "none"
            assert r.stdout == want + "\n", (n, r.stdout)


def test_the_live_ports_of_31_10_are_kept_and_pp_is_deferred():
    want = {"cam2": "10.77.9.202:7002", "cam3": "10.77.9.202:7003", "cam5": "10.77.9.202:7000",
            "cam1": "none", "cam4": "none", "cam6": "none", "cam7": "none"}
    for cam, t in want.items():
        assert _bash('cambox_jxs_target "$1"', cam).stdout.strip() == t, cam


def test_a_camera_no_venue_lists_has_no_target():
    r = _bash('cambox_jxs_target "$1"', "cam8")
    assert r.returncode == 1 and r.stdout == "" and "STRIH_CAMERAS" in r.stderr


def test_the_venue_lookup_keeps_its_two_old_answers(tmp_path):
    # the third field must not leak into the venue (remote_log_camera_venue) or the sink
    r = _bash('remote_log_camera_venue cam5; remote_log_sink_ip cam5')
    assert r.stdout.splitlines() == ["snv", "10.77.9.202"], r.stdout
    # a synthetic venue file without the JPEG XS fact (an older checkout) reads none
    (tmp_path / "strih-aa.env").write_text("STRIH_ROLE=strih\nSTRIH_IP=10.0.0.1\nSTRIH_VENUE=aa\nSTRIH_CAMERAS=2\n")
    r = _bash('cambox_jxs_target cam2; remote_log_camera_venue cam2', env={"STRIH_BOXES_DIR": str(tmp_path)})
    assert r.stdout.splitlines() == ["none", "aa"], r.stderr


def test_the_env_var_is_camera_boxs_own():
    assert 'pub const TARGET_ENV: &str = "CAMERA_BOX_JXS_TARGET";' in JXS_SEND.read_text()
    assert _bash('printf "%s\\n" "$CAMBOX_JXS_ENV"').stdout == "CAMERA_BOX_JXS_TARGET\n"


def test_target_shape():
    for ok in ("none", "10.77.9.202:7000", "1.2.3.4:65535"):
        assert _bash('cambox_jxs_target_ok "$1"', ok).returncode == 0, ok
    for bad in ("", "10.77.9.202", "10.77.9.202:0", "10.77.9.202:65536", "10.77.9.300:7000",
                "host:7000", "10.77.9.202:7000:1", "10.77.9.202:07000"):
        assert _bash('cambox_jxs_target_ok "$1"', bad).returncode != 0, bad


# --- the drop-in (setup-device STEP 7) ---------------------------------------------------------------

def test_dropin_install_writes_rewrites_and_removes(tmp_path):
    path = tmp_path / "etc/systemd/system/camera-box.service.d/jxs.conf"
    r = _bash('cambox_jxs_dropin_install "$1" "$2"', str(tmp_path), "10.77.9.202:7003")
    assert r.returncode == 0, r.stderr
    text = path.read_text()
    assert text.endswith("[Service]\nEnvironment=CAMERA_BOX_JXS_TARGET=10.77.9.202:7003\n")
    assert oct(path.stat().st_mode & 0o777) == "0o644"
    sha = _bash('cambox_jxs_sha "$1"', "10.77.9.202:7003").stdout.strip()
    assert hashlib.sha256(path.read_bytes()).hexdigest() == sha
    r = _bash('cambox_jxs_dropin_install "$1" none', str(tmp_path))
    assert r.returncode == 0 and not path.exists() and "removed" in r.stdout
    r = _bash('cambox_jxs_dropin_install "$1" none', str(tmp_path))
    assert r.returncode == 0 and "none declared" in r.stdout
    r = _bash('cambox_jxs_dropin_install "$1" "$2"', str(tmp_path), "nonsense")
    assert r.returncode == 1 and not path.exists()


def test_setup_device_resolves_after_the_log_sink_and_writes_in_step7_before_its_reload():
    s = SETUP.read_text()
    code = "\n".join(ln for ln in s.splitlines() if not ln.lstrip().startswith("#"))
    sink = code.index('REMOTE_LOG_SINK_IP="$(remote_log_sink_ip "$CAMERA_NAME")"')
    resolve = code.index('CAMBOX_JXS_TARGET="$(cambox_jxs_target "$CAMERA_NAME")"')
    first_write = code.index("\nensure_root_writable\n")
    assert sink < resolve < first_write, "resolved with the sink, before anything is written"
    step7 = s[s.index("# STEP 7: Create systemd service"):s.index("# STEP 8")]
    call = step7.index('cambox_jxs_dropin_install "" "$CAMBOX_JXS_TARGET"')
    assert call < step7.index("systemctl daemon-reload", call), "the drop-in lands before STEP 7's reload"
    assert "systemctl restart camera-box" not in step7 and "systemctl start camera-box" not in step7
    assert re.search(r'(?m)^\. "\$HERE/lib/cambox-jxs-target\.sh"', s)


# --- the gather + verdict (verify-device (ay)) ------------------------------------------------------

_SYSTEMCTL = r'''
import os, subprocess, sys
st = os.environ["FAKE_STATE"]
args = sys.argv[1:]
base = os.path.join(os.path.dirname(sys.argv[0]), "systemctl-base")


def loaded_from_dropins():
    """The LAST assignment of the var across the drop-ins, /etc masking a /run file of the same name."""
    files = {}
    for d in (os.environ["RUN_DIR"], os.environ["ETC_DIR"]):  # /etc after /run: it wins a shared name
        if os.path.isdir(d):
            for n in os.listdir(d):
                if n.endswith(".conf"):
                    files[n] = os.path.join(d, n)
    val = ""
    for n in sorted(files):
        for line in open(files[n]):
            line = line.strip()
            if line.startswith("Environment=CAMERA_BOX_JXS_TARGET="):
                val = line.split("=", 2)[2]
    return val


def read(name, default=""):
    p = os.path.join(st, name)
    return open(p).read().strip() if os.path.exists(p) else default


if args[:3] == ["show", "-p", "MainPID"]:
    print(read("mainpid", "0"))
elif args[:3] == ["show", "-p", "Environment"]:
    env = "NDI_RUNTIME_DIR_V6=/usr/lib/ndi CAMERA_BOX_GENLOCK_FPS=60"
    t = read("loaded")
    print(env + (f" CAMERA_BOX_JXS_TARGET={t}" if t else ""))
elif args[:1] == ["daemon-reload"] and not os.environ.get("FAKE_DAEMON_RELOAD_RC"):
    open(os.path.join(st, "loaded"), "w").write(loaded_from_dropins() + "\n")
elif args[:2] == ["restart", "camera-box"]:
    pid = read("mainpid", "0")
    t = read("loaded")
    env = b"NDI_RUNTIME_DIR_V6=/usr/lib/ndi\0CAMERA_BOX_GENLOCK_FPS=60\0"
    if t:
        env += f"CAMERA_BOX_JXS_TARGET={t}\0".encode()
    open(os.path.join(os.environ["PROC_DIR"], pid, "environ"), "wb").write(env)
sys.exit(subprocess.run([base] + args).returncode)
'''


def _box(tmp_path, etc=None, run=None, running="10.77.9.202:7003", root_mode="ro"):
    """A fake cambox: the persistent drop-in text `etc` (None = absent), a runtime jxs.conf setting `run`
    (None = none), camera-box running with `running` as its target (None = not set, "DOWN" = no process).
    The loaded unit env starts as what a daemon-reload of those files would give."""
    tmp_path.mkdir(parents=True, exist_ok=True)
    box = make_box(tmp_path, root=root_mode)
    stub, state = box["stub"], box["state"]
    stub.joinpath("mkdir").symlink_to(shutil.which("mkdir", path="/usr/bin:/bin"))
    stub.joinpath("systemctl").rename(stub / "systemctl-base")
    stub.joinpath("systemctl").write_text(f"#!{sys.executable}\n{_SYSTEMCTL}")
    stub.joinpath("systemctl").chmod(0o755)
    fs = state / "fs"
    etc_dir, run_dir, proc = fs / "etc", fs / "run", fs / "proc"
    for d in (etc_dir, run_dir, proc / "4242"):
        d.mkdir(parents=True)
    if etc is not None:
        (etc_dir / "jxs.conf").write_text(etc)
    if run is not None:
        (run_dir / "jxs.conf").write_text(f"[Service]\nEnvironment=CAMERA_BOX_JXS_TARGET={run}\n")
    (run_dir / "other.conf").write_text("[Service]\nEnvironment=FOO=1\n")
    if running != "DOWN":
        (state / "mainpid").write_text("4242\n")
        env = b"NDI_RUNTIME_DIR_V6=/usr/lib/ndi\0"
        if running:
            env += f"CAMERA_BOX_JXS_TARGET={running}\0".encode()
        (proc / "4242" / "environ").write_bytes(env)
    box["env"] = {"CAMBOX_JXS_DROPIN": str(etc_dir / "jxs.conf"), "CAMBOX_JXS_RUNTIME_DIR": str(run_dir),
                  "CAMBOX_JXS_PROC": str(proc)}
    box["fake"] = {"ETC_DIR": str(etc_dir), "RUN_DIR": str(run_dir), "PROC_DIR": str(proc)}
    # the loaded env = a daemon-reload of the starting files
    subprocess.run([str(stub / "systemctl"), "daemon-reload"], env={**box_env_min(box)}, check=True)
    (state / "log").write_text("")
    return box


def box_env_min(box):
    return {"PATH": str(box["stub"]), "FAKE_STATE": str(box["state"]), **box["fake"]}


def _content(t):
    return build(f'. "{LIB}"\nprintf "%s\\n" "$(cambox_jxs_dropin_content "$1")"', t)


def _program(box, t):
    return build(f'. "{LIB}"\ncambox_jxs_apply_program "$1"', t, env=box["env"])


def _gather(box):
    snippet = build(f'. "{LIB}"\ncambox_jxs_gather_remote_snippet', env=box["env"])
    return run_text(box, snippet, **box["fake"]).stdout


def _verdict(block, want):
    return _bash('cambox_jxs_verdict "$1" "$2"', block, want).stdout


def test_the_live_hand_state_fails_ay_by_name(tmp_path):
    # cam3 on 31.10.2026: only the /run drop-in, the target loaded and running
    box = _box(tmp_path, run="10.77.9.202:7003")
    block = _gather(box)
    assert "JXS_DROPIN_SHA=__ABSENT__" in block and "JXS_RUNNING=10.77.9.202:7003" in block
    v = _verdict(block, "10.77.9.202:7003")
    assert "is missing -- a reboot drops the camera to NDI only" in v
    assert "a runtime drop-in sets CAMERA_BOX_JXS_TARGET" in v
    assert "loads" not in v and "running camera-box" not in v, v


def test_ay_passes_a_provisioned_box_and_a_none_box(tmp_path):
    box = _box(tmp_path / "a", etc=_content("10.77.9.202:7003"), running="10.77.9.202:7003")
    assert _verdict(_gather(box), "10.77.9.202:7003") == "ok\n"
    box = _box(tmp_path / "b", running=None)
    assert _verdict(_gather(box), "none") == "ok\n"


def test_ay_fails_a_target_on_an_undeclared_camera_and_every_drift(tmp_path):
    box = _box(tmp_path / "a", run="10.77.9.202:7001", running="10.77.9.202:7001")
    v = _verdict(_gather(box), "none")
    assert "loads CAMERA_BOX_JXS_TARGET=10.77.9.202:7001, but the venue declares no JPEG XS link" in v
    assert "sends JPEG XS to 10.77.9.202:7001" in v and "runtime drop-in" in v
    box = _box(tmp_path / "b", etc=_content("10.77.9.202:7000"), running="10.77.9.202:7000")
    v = _verdict(_gather(box), "10.77.9.202:7003")
    assert "differs from the declared target" in v and "not 10.77.9.202:7003" in v
    box = _box(tmp_path / "c", etc=_content("10.77.9.202:7003"), running="DOWN")
    assert "could not be read" in _verdict(_gather(box), "10.77.9.202:7003")


def test_ay_verdict_fails_closed():
    assert _verdict("", "10.77.9.202:7003").count("FAIL:") == 4
    assert _verdict("JXS_DROPIN_SHA=__ABSENT__", "").startswith("FAIL: no JPEG XS target resolved")


def test_verify_device_documents_places_and_runs_ay(tmp_path):
    s = VERIFY.read_text()
    head = s[:s.find("set -euo pipefail")]
    assert "#   (ay) " in head
    usage = s[s.find("usage()"):]
    assert "(ay)" in usage[:usage.find("\nEOF")]
    ay = s.find("\n# (ay) ")
    assert s.find("\n# (ak) ") < ay < s.find("\n# (ax) ") < s.rfind("# (q) .bak cruft drift")
    block = s[ay:s.find("\n# (", ay + 10)]
    assert "fail " in block and 'warn "' not in block
    assert re.search(r'(?m)^\. "\$HERE/lib/cambox-jxs-target\.sh"', s)
    box = _box(tmp_path, etc=_content("10.77.9.202:7003"), running="10.77.9.202:7003")
    canned = _gather(box)
    prelude = ('ok() { echo "OK $1"; }\nfail() { echo "FAIL $1"; }\n'
               'ssh_box() { printf "%s" "$AY_BLOCK"; return "${AY_RC:-0}"; }\nCAMERA_NAME=cam3\n')
    for env, want in (({"AY_BLOCK": canned}, "OK JPEG XS link target 10.77.9.202:7003"),
                      ({"AY_BLOCK": "JXS_DROPIN_SHA=__ABSENT__\n"}, "FAIL JPEG XS link target:"),
                      ({"AY_BLOCK": "", "AY_RC": "255"}, "FAIL could not reach the box")):
        r = _bash(prelude + block, env=env)
        assert r.returncode == 0 and r.stdout.startswith(want), (env, r.stdout, r.stderr)


# --- the live-box apply ---------------------------------------------------------------------------------

def test_apply_on_the_live_hand_state_writes_in_one_window_and_restarts_nothing(tmp_path):
    box = _box(tmp_path, run="10.77.9.202:7003")
    r = run_text(box, _program(box, "10.77.9.202:7003"), strict="set -euo pipefail", **box["fake"])
    assert r.returncode == 0, r.stdout + r.stderr
    lines = log(box)
    rw, ro = index(lines, "mount -o remount,rw /"), index(lines, "mount -o remount,ro /")
    reload_ = index(lines, "systemctl daemon-reload", ro)
    assert rw < ro < reload_
    assert not any(ln.startswith("systemctl restart") for ln in lines), lines
    assert starts_on_rw(lines) == [] and root(box) == "ro"
    assert pathlib.Path(box["env"]["CAMBOX_JXS_DROPIN"]).read_text() == _content("10.77.9.202:7003")
    assert not (pathlib.Path(box["env"]["CAMBOX_JXS_RUNTIME_DIR"]) / "jxs.conf").exists()
    assert (pathlib.Path(box["env"]["CAMBOX_JXS_RUNTIME_DIR"]) / "other.conf").exists()
    assert "READBACK" in r.stdout and "OK CAMERA_BOX_JXS_TARGET=10.77.9.202:7003" in r.stdout
    assert "not restarted" in r.stdout
    assert _verdict(_gather(box), "10.77.9.202:7003") == "ok\n"
    # an idempotent re-run opens no window and restarts nothing
    (box["state"] / "log").write_text("")
    r = run_text(box, _program(box, "10.77.9.202:7003"), strict="set -euo pipefail", **box["fake"])
    assert r.returncode == 0, r.stderr
    assert not any(ln.startswith("mount") for ln in log(box)), log(box)
    assert "drop-in in-place" in r.stdout


def test_apply_restarts_camera_box_after_the_close_only_on_a_different_target(tmp_path):
    box = _box(tmp_path, run="10.77.9.202:7000", running="10.77.9.202:7000")
    r = run_text(box, _program(box, "10.77.9.202:7005"), strict="set -euo pipefail", **box["fake"])
    assert r.returncode == 0, r.stdout + r.stderr
    lines = log(box)
    ro = index(lines, "mount -o remount,ro /")
    restart = index(lines, "systemctl restart camera-box", ro)
    assert index(lines, "systemctl daemon-reload", ro) < restart
    assert starts_on_rw(lines) == [] and root(box) == "ro"
    assert "restarted camera-box (10.77.9.202:7000 -> 10.77.9.202:7005)" in r.stdout
    assert _verdict(_gather(box), "10.77.9.202:7005") == "ok\n"


def test_apply_takes_a_hand_target_off_a_none_camera_without_a_window(tmp_path):
    box = _box(tmp_path, run="10.77.9.202:7001", running="10.77.9.202:7001")
    r = run_text(box, _program(box, "none"), strict="set -euo pipefail", **box["fake"])
    assert r.returncode == 0, r.stdout + r.stderr
    lines = log(box)
    assert not any(ln.startswith("mount") for ln in lines), lines
    assert index(lines, "systemctl daemon-reload") < index(lines, "systemctl restart camera-box")
    assert _verdict(_gather(box), "none") == "ok\n"


def test_apply_removes_a_persistent_dropin_of_an_undeclared_camera(tmp_path):
    box = _box(tmp_path, etc=_content("10.77.9.202:7001"), running="10.77.9.202:7001")
    r = run_text(box, _program(box, "none"), strict="set -euo pipefail", **box["fake"])
    assert r.returncode == 0, r.stdout + r.stderr
    assert not pathlib.Path(box["env"]["CAMBOX_JXS_DROPIN"]).exists()
    assert "drop-in remove" in r.stdout and root(box) == "ro"
    assert _verdict(_gather(box), "none") == "ok\n"


def test_apply_on_a_busy_root_fails_loud_and_reloads_restarts_nothing(tmp_path):
    box = _box(tmp_path, run="10.77.9.202:7000", running="10.77.9.202:7000")
    r = run_text(box, _program(box, "10.77.9.202:7005"), strict="set -euo pipefail", FAKE_RO_FAIL="1",
                 **box["fake"])
    assert r.returncode != 0
    assert "root is NOT read-only" in r.stderr
    lines = log(box)
    assert not any(ln.startswith(("systemctl restart", "systemctl daemon-reload")) for ln in lines), lines


def test_apply_never_starts_a_stopped_camera_box(tmp_path):
    box = _box(tmp_path, run="10.77.9.202:7003", running="DOWN")
    r = run_text(box, _program(box, "10.77.9.202:7003"), strict="set -euo pipefail", **box["fake"])
    assert r.returncode == 0, r.stdout + r.stderr
    assert not any(ln.startswith(("systemctl restart", "systemctl start")) for ln in log(box))
    assert "camera-box is not running" in r.stdout


def test_apply_on_a_stuck_writable_root_closes_it_before_anything(tmp_path):
    box = _box(tmp_path, etc=_content("10.77.9.202:7003"), running="10.77.9.202:7003", root_mode="rw")
    r = run_text(box, _program(box, "10.77.9.202:7003"), strict="set -euo pipefail", **box["fake"])
    assert r.returncode == 0, r.stdout + r.stderr
    lines = log(box)
    assert index(lines, "mount -o remount,ro /") < index(lines, "systemctl daemon-reload")
    assert root(box) == "ro"


def test_apply_refuses_a_bad_target():
    r = _bash('cambox_jxs_apply_program "$1"', "10.77.9.202")
    assert r.returncode == 1 and r.stdout == ""


def test_the_cli_prints_the_target_and_the_program():
    r = subprocess.run(["bash", str(LIB), "--target", "cam5"], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0 and r.stdout == "10.77.9.202:7000\n", r.stderr
    r = subprocess.run(["bash", str(LIB), "--apply-program", "cam5"], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0 and "_cjx_want=10.77.9.202:7000" in r.stdout and "_cjx_main </dev/null" in r.stdout
    assert subprocess.run(["bash", "-n"], input=r.stdout, text=True).returncode == 0
    r = subprocess.run(["bash", str(LIB), "--target", "cam9"], capture_output=True, text=True, timeout=60)
    assert r.returncode != 0
