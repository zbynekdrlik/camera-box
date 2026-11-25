"""issue 1553 (design issuecomment-7172341290): the strih / stream box's sender egress pacing, persistent.

The box fact STRIH_EGRESS_PACING_RATE (all four boxes 900mbit), scripts/lib/strih-egress-pacing.sh
(the generated boot script, systemd/strih-egress-pacing.service, setup-strih step 11b's install and
verify-strih item 16's rows). The libs run under the callers' `set -euo pipefail`; `tc` and `systemctl`
are fakes on PATH (the fake tc keeps a qdisc tree in a JSON state file with the kernel's rules: mq only on
a multiqueue device, a class must exist, the minor read as HEX), /sys and the install root are temp trees
through the lib's seams. Values reach bash as ARGUMENTS, never inside the script text. The JSON fixtures
are real `tc -j qdisc show` output of a 7.0 kernel (a dummy device in a network namespace, 25.11.2026).
"""
import json
import os
import pathlib
import re
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
LIB = SCRIPTS / "lib" / "strih-egress-pacing.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
UNIT = REPO / "systemd" / "strih-egress-pacing.service"
BOXES = SCRIPTS / "strih-boxes"
BOX_NAMES = ("strih-snv", "strih-pp", "stream-snv", "stream-pp")
LEAKY = ("STRIH_LX_IP", "STRIH_NIC_IFACE", "STRIH_BOXES_DIR", "OBS_FLEET", "STRIH_EGRESS_PACING_ROOT",
         "STRIH_EGRESS_PACING_SYS", "STRIH_EGRESS_PACING_IPADDR", "STRIH_EGRESS_PACING_RETRY_SLEEP_S")
WANT_BYTES = 112500000  # 900mbit in bytes/s, what tc -j prints

# Real kernel output (3-queue dummy, mq + fq 900mbit / 4000 / 20000 under 1:1..1:3).
TREE_3Q = ('[{"kind":"mq","handle":"1:","root":true,"options":{}},{"kind":"fq","handle":"8007:","parent":"1:2",'
           '"options":{"limit":20000,"flow_limit":4000,"buckets":1024,"maxrate":112500000}},{"kind":"fq",'
           '"handle":"8006:","parent":"1:1","options":{"limit":20000,"flow_limit":4000,"buckets":1024,'
           '"maxrate":112500000}},{"kind":"fq","handle":"8008:","parent":"1:3","options":{"limit":20000,'
           '"flow_limit":4000,"buckets":1024,"maxrate":112500000}}]')
TREE_1Q = ('[{"kind":"fq","handle":"8009:","root":true,"refcnt":2,"options":{"limit":20000,"flow_limit":4000,'
           '"buckets":1024,"maxrate":112500000}}]')
NOQUEUE = '[{"kind":"noqueue","handle":"0:","root":true,"refcnt":2,"options":{}}]'


def _decimal_loop_tree(queues):
    """What the hand fix's decimal `parent 1:N` loop leaves: fq on 1:1..1:9 (+ 1:10 = queue 16), the rest
    the default fq_codel (measured on a 12-queue dummy: `Specified class not found` for 1:10..1:12)."""
    tree = [{"kind": "mq", "handle": "1:", "root": True, "options": {}}]
    for i in range(1, queues + 1):
        cls = "1:%x" % i
        paced = i <= 9 or cls in ("1:%d" % n for n in range(10, queues + 1))
        if paced:
            tree.append({"kind": "fq", "parent": cls, "options": {"limit": 20000, "flow_limit": 4000, "maxrate": WANT_BYTES}})
        else:
            tree.append({"kind": "fq_codel", "parent": cls, "options": {}})
    return json.dumps(tree)


FAKE_TC = r'''#!/usr/bin/env python3
import json, os, sys
state = os.environ["FAKE_TC_STATE"]
queues = int(os.environ.get("FAKE_TC_QUEUES", "1"))
with open(os.environ["FAKE_TC_LOG"], "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
if os.environ.get("FAKE_TC_FAIL") and "replace" in sys.argv:
    sys.stderr.write("RTNETLINK answers: Invalid argument\n"); sys.exit(2)
tree = json.load(open(state)) if os.path.exists(state) else [{"kind": "noqueue", "handle": "0:", "root": True, "options": {}}]
a = sys.argv[1:]
def rate(v):
    v = v.lower()
    for u, m in (("gbit", 10**9), ("mbit", 10**6)):
        if v.endswith(u):
            return int(v[:-len(u)]) * m // 8
    raise SystemExit("bad rate " + v)
def fq(opts):
    o = {"limit": 10000, "flow_limit": 100, "buckets": 1024}
    i = 0
    while i < len(opts):
        k = opts[i]
        if k == "maxrate": o["maxrate"] = rate(opts[i + 1])
        elif k in ("flow_limit", "limit"): o[k] = int(opts[i + 1])
        else: raise SystemExit("unknown fq option " + k)
        i += 2
    return o
if a[:2] == ["-j", "qdisc"] and a[2] == "show":
    print(json.dumps(tree)); sys.exit(0)
assert a[:2] == ["qdisc", "replace"] and a[2] == "dev", a
if a[4] == "root":
    if a[5:8] == ["handle", "1:", "mq"]:
        if queues < 2:
            sys.stderr.write("RTNETLINK answers: Operation not supported\n"); sys.exit(2)
        if not (tree and tree[0].get("kind") == "mq"):
            tree = [{"kind": "mq", "handle": "1:", "root": True, "options": {}}] + [
                {"kind": "fq_codel", "parent": "1:%x" % i, "options": {}} for i in range(1, queues + 1)]
    elif a[5] == "fq":
        tree = [{"kind": "fq", "handle": "8001:", "root": True, "options": fq(a[6:])}]
    else:
        raise SystemExit("unhandled " + " ".join(a))
elif a[4] == "parent":
    cls, kind = a[5], a[6]
    assert kind == "fq", a
    hit = [q for q in tree if q.get("parent") == cls]
    if not (tree and tree[0].get("kind") == "mq") or not hit:
        sys.stderr.write("Error: Specified class not found.\n"); sys.exit(2)
    hit[0].clear(); hit[0].update({"kind": "fq", "parent": cls, "options": fq(a[7:])})
json.dump(tree, open(state, "w"))
'''

FAKE_SYSTEMCTL = r'''#!/bin/bash
echo "$*" >> "$FAKE_SYSTEMCTL_LOG"
st="$FAKE_SYSTEMCTL_DIR/enabled"
case "$1" in
  is-enabled) [ "${2:-}" = -q ] && { [ -f "$st" ]; exit $?; }; if [ -f "$st" ]; then echo enabled; else echo disabled; exit 1; fi ;;
  enable) touch "$st" ;;
  disable) rm -f "$st" ;;
  restart) bash "$STRIH_EGRESS_PACING_ROOT/usr/local/sbin/strih-egress-pacing" ;;
  daemon-reload) ;;
esac
'''


def _exe(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


def _world(tmp_path, queues=3, nic="enp5s0f1"):
    """A fake box: install root, /sys with NIC + its tx queues, fake tc + systemctl on PATH."""
    root = tmp_path / "root"
    root.mkdir()
    sysd = tmp_path / "sys"
    q = sysd / "class" / "net" / nic / "queues"
    q.mkdir(parents=True)
    for i in range(queues):
        (q / ("tx-%d" % i)).mkdir()
        (q / ("rx-%d" % i)).mkdir()
    bind = tmp_path / "bin"
    bind.mkdir()
    _exe(bind / "tc", FAKE_TC)
    _exe(bind / "systemctl", FAKE_SYSTEMCTL)
    sctl = tmp_path / "sctl"
    sctl.mkdir()
    env = {
        "STRIH_EGRESS_PACING_ROOT": str(root),
        "STRIH_EGRESS_PACING_SYS": str(sysd),
        "STRIH_EGRESS_PACING_RETRY_SLEEP_S": "0",
        "STRIH_NIC_IFACE": nic,
        "FAKE_TC_STATE": str(tmp_path / "tc.json"),
        "FAKE_TC_LOG": str(tmp_path / "tc.log"),
        "FAKE_TC_QUEUES": str(queues),
        "FAKE_SYSTEMCTL_LOG": str(tmp_path / "systemctl.log"),
        "FAKE_SYSTEMCTL_DIR": str(sctl),
    }
    return {"root": root, "sys": sysd, "bin": bind, "env": env, "tmp": tmp_path, "nic": nic}


def _run(body, *args, env=None, path_prefix=None):
    harness = ("set -euo pipefail\n" + '. "%s"\n. "%s"\n' % (FACTS, PROVISION) +
               'fail() { echo "FAIL: $*" >&2; return 1; }\n'
               'ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n' + body)
    full = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if path_prefix:
        full["PATH"] = "%s:%s" % (path_prefix, full["PATH"])
    if env:
        full.update(env)
    return subprocess.run(["bash", "-c", harness, "harness", *args], capture_output=True, text=True,
                          env=full, cwd=str(REPO), timeout=60)


def _w(world, body, *args, box="strih-pp"):
    return _run('strih_box_load "$1"; shift\n' + body, box, *args, env=world["env"], path_prefix=str(world["bin"]))


def _lines(path):
    return path.read_text().splitlines() if path.exists() else []


# --- the fact --------------------------------------------------------------------------------------------

def test_the_fact_is_the_last_key_and_every_box_paces_at_900mbit():
    r = _run("strih_box_fact_keys | tail -n 1")
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "STRIH_EGRESS_PACING_RATE"
    for box in BOX_NAMES:
        r = _run('strih_box_load "$1"; strih_egress_pacing_rate', box)
        assert r.returncode == 0, (box, r.stderr)
        assert r.stdout.strip() == "900mbit", box


def _load_with(tmp_path, value):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text())
    body = (d / "strih-pp.env").read_text()
    assert body.count("STRIH_EGRESS_PACING_RATE=900mbit\n") == 1
    (d / "strih-pp.env").write_text(body.replace("STRIH_EGRESS_PACING_RATE=900mbit\n", "STRIH_EGRESS_PACING_RATE=%s\n" % value))
    return _run('strih_box_load strih-pp; strih_egress_pacing_rate', env={"STRIH_BOXES_DIR": str(d)})


def test_the_loader_takes_none_and_mbit_gbit_rates(tmp_path):
    for i, v in enumerate(("none", "900mbit", "1gbit", "2500mbit")):
        r = _load_with(tmp_path / str(i), v)
        assert r.returncode == 0, (v, r.stderr)
        assert r.stdout.strip() == v


def test_the_loader_refuses_anything_else_by_name(tmp_path):
    for i, v in enumerate(("900", "0mbit", "0900mbit", "900Mbit", "900kbit", "1234567mbit", "", "fast")):
        r = _load_with(tmp_path / str(i), v)
        assert r.returncode != 0, v
        assert "STRIH_EGRESS_PACING_RATE" in r.stderr, (v, r.stderr)


def test_rate_bytes_is_what_tc_json_prints():
    r = _run('strih_egress_pacing_rate_bytes 900mbit; strih_egress_pacing_rate_bytes 1gbit; '
             'strih_egress_pacing_rate_bytes none || echo none-refused')
    assert r.stdout.split() == ["112500000", "125000000", "none-refused"], r.stderr


# --- the verdict over real kernel JSON -----------------------------------------------------------------------

def _verdict(rate, queues, text):
    r = _run('strih_egress_pacing_tree_verdict "$1" "$2" "$3"', rate, str(queues), text)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_verdict_ok_on_the_mq_tree_and_on_a_single_queue_root_fq():
    assert _verdict("900mbit", 3, TREE_3Q).startswith("OK mq 1: + fq on 3 tx queues")
    assert _verdict("900mbit", 1, TREE_1Q).startswith("OK root fq (1 tx queue)")


def test_verdict_names_the_queues_the_decimal_loop_leaves_unpaced():
    v = _verdict("900mbit", 12, _decimal_loop_tree(12))
    assert v.startswith("DRIFT"), v
    for cls in ("1:a", "1:b", "1:c"):
        assert "%s has fq_codel, not fq" % cls in v, v
    assert "1:9 " not in v


def test_verdict_drift_on_a_wrong_value_a_missing_queue_and_no_tree():
    assert "fq maxrate 50000000 bytes/s (want 900mbit" in _verdict("900mbit", 3, TREE_3Q.replace("112500000", "50000000", 1))
    assert "fq flow_limit 2000 (want 4000)" in _verdict("900mbit", 1, TREE_1Q.replace('"flow_limit":4000', '"flow_limit":2000'))
    four = _verdict("900mbit", 4, TREE_3Q)
    assert "1:4 has nothing, not fq -- tx queue 4 is UNPACED" in four
    assert _verdict("900mbit", 3, NOQUEUE).startswith("DRIFT the root is noqueue, not mq 1:")
    assert _verdict("900mbit", 1, NOQUEUE).startswith("DRIFT the root is noqueue, not fq")
    assert _verdict("900mbit", 3, "").startswith("UNKNOWN")
    assert "an extra qdisc under 1:3" in _verdict("900mbit", 2, TREE_3Q)


# --- the boot script --------------------------------------------------------------------------------------

def _script(world, rate="900mbit"):
    r = _run('strih_egress_pacing_script_text "$1" "$2"', world["nic"], rate)
    assert r.returncode == 0, r.stderr
    p = world["tmp"] / "boot.sh"
    p.write_text(r.stdout)
    return p


def _boot(world, path, **extra):
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    env["PATH"] = "%s:%s" % (world["bin"], env["PATH"])
    env.update(world["env"])
    env.update(extra)
    return subprocess.run(["bash", str(path)], capture_output=True, text=True, env=env, timeout=60)


def test_the_boot_script_paces_every_queue_by_its_hex_class(tmp_path):
    w = _world(tmp_path, queues=16)
    r = _boot(w, _script(w))
    assert r.returncode == 0, r.stderr
    parents = [l.split()[5] for l in _lines(tmp_path / "tc.log") if " parent " in l]
    assert parents == ["1:%x" % i for i in range(1, 17)]
    tree = (tmp_path / "tc.json").read_text()
    assert _verdict("900mbit", 16, tree).startswith("OK mq 1: + fq on 16 tx queues"), tree
    assert "on 16 tx queue(s)" in r.stdout


def test_the_boot_script_uses_a_root_fq_on_a_single_queue_nic_and_reruns_idempotently(tmp_path):
    w = _world(tmp_path, queues=1)
    s = _script(w)
    for _ in range(2):
        r = _boot(w, s)
        assert r.returncode == 0, r.stderr
    assert _verdict("900mbit", 1, (tmp_path / "tc.json").read_text()).startswith("OK root fq")
    assert not any(" mq" in l for l in _lines(tmp_path / "tc.log"))


def test_the_boot_script_waits_for_an_absent_nic_then_fails_loud(tmp_path):
    w = _world(tmp_path, queues=2, nic="rig0")
    s = _script(w)
    import shutil
    shutil.rmtree(w["sys"] / "class" / "net" / "rig0")
    r = _boot(w, s)
    assert r.returncode == 1
    assert "FAILED -- the rig NIC rig0 is not present after 30 attempts" in r.stderr
    assert r.stderr.count("not present yet") == 1
    assert not (tmp_path / "tc.log").exists()


def test_a_failed_tc_step_exits_2_naming_it(tmp_path):
    w = _world(tmp_path, queues=2)
    r = _boot(w, _script(w), FAKE_TC_FAIL="1")
    assert r.returncode == 2
    assert "tc root mq on enp5s0f1 FAILED" in r.stderr


def test_the_script_renderer_refuses_a_bad_nic_or_rate():
    for nic, rate in (("enp5s0f1", "none"), ("enp5s0f1", "900"), ("a;b", "900mbit"), ("", "900mbit")):
        r = _run('strih_egress_pacing_script_text "$1" "$2" || echo REFUSED', nic, rate)
        assert r.stdout.strip() == "REFUSED", (nic, rate, r.stdout)


def test_the_unit_is_the_checked_in_file():
    r = _run("strih_egress_pacing_unit_text")
    assert r.returncode == 0, r.stderr
    assert r.stdout == UNIT.read_text()
    u = UNIT.read_text()
    for line in ("Type=oneshot", "RemainAfterExit=yes", "After=network-online.target", "Wants=network-online.target",
                 "ExecStart=/usr/local/sbin/strih-egress-pacing", "Restart=on-failure", "RestartPreventExitStatus=2",
                 "StartLimitIntervalSec=0", "WantedBy=multi-user.target"):
        assert line in u.splitlines(), line


# --- setup-strih step 11b: the install ------------------------------------------------------------------------

def test_install_writes_enables_and_applies_live_then_a_rerun_touches_nothing(tmp_path):
    w = _world(tmp_path, queues=3)
    r = _w(w, "strih_egress_pacing_install")
    assert r.returncode == 0, r.stdout + r.stderr
    script = w["root"] / "usr/local/sbin/strih-egress-pacing"
    unit = w["root"] / "etc/systemd/system/strih-egress-pacing.service"
    assert oct(script.stat().st_mode & 0o777) == "0o755" and oct(unit.stat().st_mode & 0o777) == "0o644"
    assert unit.read_text() == UNIT.read_text()
    assert "nic=enp5s0f1\nrate=900mbit\n" in script.read_text()
    calls = _lines(tmp_path / "systemctl.log")
    assert "daemon-reload" in calls and "enable strih-egress-pacing.service" in calls
    assert calls.index("daemon-reload") < calls.index("restart strih-egress-pacing.service")
    assert _verdict("900mbit", 3, (tmp_path / "tc.json").read_text()).startswith("OK")
    (tmp_path / "systemctl.log").unlink()
    m1 = script.stat().st_mtime_ns
    r = _w(w, "strih_egress_pacing_install")
    assert r.returncode == 0, r.stderr
    assert "already paced" in r.stdout
    calls = _lines(tmp_path / "systemctl.log")
    assert not any(c.startswith(("restart", "daemon-reload", "enable")) for c in calls), calls
    assert script.stat().st_mtime_ns == m1


def test_install_reapplies_a_lost_tree_without_rewriting_files(tmp_path):
    w = _world(tmp_path, queues=3)
    assert _w(w, "strih_egress_pacing_install").returncode == 0
    (tmp_path / "tc.json").unlink()          # a re-added adapter: the tree is gone
    (tmp_path / "systemctl.log").unlink()
    r = _w(w, "strih_egress_pacing_install")
    assert r.returncode == 0, r.stderr
    assert _lines(tmp_path / "systemctl.log") == ["is-enabled -q strih-egress-pacing.service", "restart strih-egress-pacing.service"]
    assert "applied live" in r.stdout


def test_install_with_none_removes_only_ours(tmp_path):
    w = _world(tmp_path, queues=3)
    r = _w(w, "strih_egress_pacing_install", box="strih-pp")
    assert r.returncode == 0
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text().replace("STRIH_EGRESS_PACING_RATE=900mbit\n", "STRIH_EGRESS_PACING_RATE=none\n"))
    env = dict(w["env"], STRIH_BOXES_DIR=str(d))
    r = _run('strih_box_load strih-pp; strih_egress_pacing_install', env=env, path_prefix=str(w["bin"]))
    assert r.returncode == 0, r.stderr
    assert not (w["root"] / "usr/local/sbin/strih-egress-pacing").exists()
    assert not (w["root"] / "etc/systemd/system/strih-egress-pacing.service").exists()
    assert "disable strih-egress-pacing.service" in _lines(tmp_path / "systemctl.log")
    r = _run('strih_box_load strih-pp; strih_egress_pacing_install', env=env, path_prefix=str(w["bin"]))
    assert "SKIP egress pacing" in r.stdout
    # a file of that name without our marker is never removed
    other = w["root"] / "usr/local/sbin/strih-egress-pacing"
    other.write_text("#!/bin/bash\n# an operator's own\n")
    r = _run('strih_box_load strih-pp; strih_egress_pacing_install', env=env, path_prefix=str(w["bin"]))
    assert other.exists() and "SKIP" in r.stdout


def test_install_fails_by_name_when_the_rig_nic_cannot_be_resolved(tmp_path):
    w = _world(tmp_path)
    env = dict(w["env"])
    del env["STRIH_NIC_IFACE"]
    env["STRIH_EGRESS_PACING_IPADDR"] = ""
    r = _run('strih_box_load strih-pp; strih_egress_pacing_install || echo RC=$?', env=env, path_prefix=str(w["bin"]))
    assert "RC=1" in r.stdout
    assert "cannot resolve the rig NIC" in r.stderr
    assert not (w["root"] / "usr/local/sbin/strih-egress-pacing").exists()


# --- verify-strih item 16: the grade ----------------------------------------------------------------------------

def _grade(w, box="strih-pp"):
    r = _w(w, "strih_egress_pacing_grade_report", box=box)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_grade_passes_after_the_install(tmp_path):
    w = _world(tmp_path, queues=3)
    assert _w(w, "strih_egress_pacing_install").returncode == 0
    out = _grade(w)
    assert "PASS (egress-pacing-install) strih-egress-pacing installed + enabled (dev enp5s0f1, maxrate 900mbit)" in out
    assert "PASS (egress-pacing) enp5s0f1 mq 1: + fq on 3 tx queues" in out
    assert "FAIL" not in out


def test_grade_fails_a_hand_tree_without_the_unit(tmp_path):
    w = _world(tmp_path, queues=3)
    (tmp_path / "tc.json").write_text(TREE_3Q)
    out = _grade(w)
    assert "FAIL (egress-pacing-install)" in out and "re-run setup-strih.sh step 11b" in out
    assert "PASS (egress-pacing) enp5s0f1" in out


def test_grade_fails_a_lost_tree_a_stale_script_and_a_disabled_unit(tmp_path):
    w = _world(tmp_path, queues=3)
    assert _w(w, "strih_egress_pacing_install").returncode == 0
    (tmp_path / "tc.json").write_text(_decimal_loop_tree(3).replace('"maxrate": 112500000', '"maxrate": 50000000', 1))
    out = _grade(w)
    assert "FAIL (egress-pacing) enp5s0f1:" in out and "fix: systemctl restart strih-egress-pacing" in out
    script = w["root"] / "usr/local/sbin/strih-egress-pacing"
    script.write_text(script.read_text().replace("rate=900mbit", "rate=400mbit"))
    assert "is missing or not the render for dev enp5s0f1 maxrate 900mbit" in _grade(w)
    assert _w(w, "strih_egress_pacing_install").returncode == 0
    (tmp_path / "sctl" / "enabled").unlink()
    assert "is 'disabled', not enabled" in _grade(w)


def test_grade_none_is_a_note_and_our_leftover_a_fail(tmp_path):
    w = _world(tmp_path)
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text().replace("STRIH_EGRESS_PACING_RATE=900mbit\n", "STRIH_EGRESS_PACING_RATE=none\n"))
    env = dict(w["env"], STRIH_BOXES_DIR=str(d))
    r = _run('strih_box_load strih-pp; strih_egress_pacing_grade_report', env=env, path_prefix=str(w["bin"]))
    assert r.stdout.startswith("NOTE (egress-pacing) STRIH_EGRESS_PACING_RATE none"), r.stdout
    (w["root"] / "etc/systemd/system").mkdir(parents=True)
    (w["root"] / "etc/systemd/system/strih-egress-pacing.service").write_text("x\n")
    r = _run('strih_box_load strih-pp; strih_egress_pacing_grade_report', env=env, path_prefix=str(w["bin"]))
    assert "FAIL (egress-pacing) STRIH_EGRESS_PACING_RATE none but strih-egress-pacing is installed" in r.stdout


# --- wiring + budgets + the one declaration --------------------------------------------------------------------

def test_setup_and_verify_call_it_once_on_one_line_within_budget():
    s, v = SETUP.read_text(), VERIFY.read_text()
    assert s.count("strih_egress_pacing_install") == 1
    line = [l for l in s.splitlines() if "strih_egress_pacing_install" in l][0]
    assert "|| fail" in line
    assert s.index('step "11b"') < s.index("strih_egress_pacing_install") < s.index("step 12 ")
    assert v.count("strih_egress_pacing_grade_report") == 1
    line = [l for l in v.splitlines() if "strih_egress_pacing_grade_report" in l][0]
    assert '|| bad "(egress-pacing)' in line
    assert len(s.splitlines()) <= 1000 and len(v.splitlines()) <= 1000
    assert len(FACTS.read_text().splitlines()) <= 1000


def test_the_lib_is_sourced_by_strih_provision():
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-egress-pacing.sh"' in PROVISION.read_text()
    r = _run("declare -F strih_egress_pacing_install strih_egress_pacing_grade_report")
    assert r.returncode == 0, r.stderr


def test_the_fq_limits_are_declared_once():
    hits = []
    for p in list(SCRIPTS.rglob("*.sh")) + list((REPO / "systemd").glob("*")):
        if p.is_file() and p != LIB and re.search(r"flow_limit 4000", p.read_text(errors="replace")):
            hits.append(p.name)
    assert hits == [], hits
    lib = LIB.read_text()
    assert 'STRIH_EGRESS_PACING_FLOW_LIMIT="4000"' in lib and 'STRIH_EGRESS_PACING_LIMIT="20000"' in lib
