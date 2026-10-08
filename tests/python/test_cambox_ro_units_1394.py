#!/usr/bin/env python3
"""issue 1394 -- every cambox read `degraded`: four units fail on the read-only root.

Live (issuecomment-6057856766, 8.10.2026 ~10:26Z, cam1/cam2/cam5): `systemctl is-system-running` =
`degraded`, the same four failed units on each box:
  - logrotate.service fails every 15 min (the issue-679 timer): its state file
    /var/lib/logrotate/status sits on the read-only root, so /var/log is not rotated at all;
  - apt-daily.service / apt-daily-upgrade.service exit 2 on every timer pass (/var/lib/apt is
    read-only); setup-device.sh only DISABLED their timers, and a package upgrade re-enables them;
  - cambox-netconsole.service (issue 1311) failed once at boot and never retried, so no kernel
    printk reaches dev1 since that boot.

The main's design (issuecomment-6057869233, Approach 1):
  - the logrotate drop-in BASE moves into the shared read-only-root canon scripts/lib/ro-root.sh;
    setup-device writes it, the handheld SBC builds its drop-in from the same base + its Armbian
    ramlog resets (byte-identical to before);
  - setup-device MASKS the four apt units;
  - the netconsole unit retries (Restart=on-failure, RestartSec=30, StartLimitIntervalSec=0);
  - verify-device grades all of it ((ar), and (ak) names a failed netconsole's Result);
  - a dry-run-first live-apply program (scripts/lib/cambox-ro-units.sh +
    scripts/cambox-ro-units-apply.sh) writes the three inside ONE verified ro window
    (scripts/lib/ro-window.sh) and starts nothing before the close.

Tier-0: stdlib + bash only, no rig. The apply program runs on the shared fake read-only-root box
(tests/python/ro_window_fakes_1407.py).
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import build, index, log, make_box, root, run_text, starts_on_rw  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib"
RO_ROOT = LIB / "ro-root.sh"
SBC = LIB / "bkshading-sbc-runtime.sh"
REMOTE_LOG = LIB / "remote-logging.sh"
UNITS = LIB / "cambox-ro-units.sh"
SETUP = REPO / "scripts" / "setup-device.sh"
VERIFY = REPO / "scripts" / "verify-device.sh"
CLI = REPO / "scripts" / "cambox-ro-units-apply.sh"

APT_UNITS = ["apt-daily.timer", "apt-daily-upgrade.timer", "apt-daily.service", "apt-daily-upgrade.service"]
LR_EXEC = "ExecStart=/usr/sbin/logrotate --state /run/logrotate.status /etc/logrotate.conf"
LR_RELPATH = "logrotate.service.d/zz-camera-box-ro-root.conf"

# The SBC drop-in as it was before issue 1394 (9462b2b35, live on handheld-1): the shared base must
# reproduce these bytes exactly.
SBC_GOLDEN = (
    "# Written by scripts/bkshading-provision-sbc.sh --install (issue 808): the root is read-only,\n"
    "# so logrotate keeps its state in /run, and the armbian-ramlog steps (the unit is masked) go.\n"
    "[Service]\n"
    "ExecStartPre=\n"
    "ExecStartPost=\n"
    "ExecStart=\n"
    "ExecStart=/usr/sbin/logrotate --state /run/logrotate.status /etc/logrotate.conf\n"
)
SBC_GOLDEN_SHA = "31a8605a1932bcb885deb078329d9bb7e47e92d345caa9acb17b2687e7df7c76"


def _bash(script, env=None, cwd=None):
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=e, cwd=cwd,
                          timeout=120)


def _lib(lib, snippet, env=None):
    """Source LIB under the strictest caller mode and run SNIPPET; asserts exit 0."""
    r = _bash(f'set -euo pipefail\n. "{lib}"\n{snippet}', env=env)
    assert r.returncode == 0, (snippet, r.returncode, r.stdout, r.stderr)
    assert "unbound variable" not in r.stderr, r.stderr
    return r.stdout


def _sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def _code(text):
    return "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))


# =================================================================================================
# 1. the shared read-only-root canon: the logrotate base + the apt units (scripts/lib/ro-root.sh)
# =================================================================================================

def test_ro_root_logrotate_base_keeps_the_state_in_run():
    assert _lib(RO_ROOT, "ro_root_logrotate_state_file") == "/run/logrotate.status\n"
    # ExecStart= first: a oneshot runs EVERY ExecStart line, so the stock one must be cleared.
    assert _lib(RO_ROOT, "ro_root_logrotate_dropin_service") == f"[Service]\nExecStart=\n{LR_EXEC}\n"
    # a box's own resets go right after the section header
    assert _lib(RO_ROOT, 'ro_root_logrotate_dropin_service "A=" "B="') == f"[Service]\nA=\nB=\nExecStart=\n{LR_EXEC}\n"


def test_cambox_logrotate_dropin_is_the_base_under_a_comment_header():
    content = _lib(RO_ROOT, "ro_root_logrotate_dropin_content")
    base = _lib(RO_ROOT, "ro_root_logrotate_dropin_service")
    head = [ln for ln in content.splitlines(keepends=True) if ln.startswith("#")]
    assert head and content == "".join(head) + base, content
    assert "issue 1394" in "".join(head)
    assert _lib(RO_ROOT, "ro_root_logrotate_dropin_relpath") == LR_RELPATH + "\n"


def test_ro_root_names_the_four_apt_units():
    assert _lib(RO_ROOT, "ro_root_masked_apt_units").split() == APT_UNITS


def test_sbc_dropin_stays_byte_identical_and_is_built_from_the_shared_base():
    out = _lib(SBC, "bkshading_sbc_logrotate_dropin_content")  # ro-root.sh NOT sourced first: lazy
    assert out == SBC_GOLDEN
    assert _sha(out) == SBC_GOLDEN_SHA
    text = SBC.read_text()
    assert 'ro_root_logrotate_dropin_service "ExecStartPre=" "ExecStartPost="' in text
    assert "--state /run/logrotate.status" not in _code(text), "the ExecStart override is typed only in ro-root.sh"


def test_the_sbc_lib_still_sources_alone_the_way_the_wifi_heal_installs_it(tmp_path):
    # bkshading-provision-sbc.sh installs bkshading-sbc-runtime.sh ALONE (no ro-root.sh) under
    # /usr/local/lib/bkshading/lib/ for the WiFi heal: a load-time source of ro-root.sh would kill it.
    (tmp_path / "lib").mkdir()
    shutil.copy(SBC, tmp_path / "lib" / SBC.name)
    r = _bash(f'set -euo pipefail\n. "{tmp_path / "lib" / SBC.name}"\nbkshading_sbc_masked_units')
    assert r.returncode == 0, r.stderr
    assert "armbian-ramlog.service" in r.stdout


# =================================================================================================
# 2. setup-device.sh: the drop-in in STEP 18's region, the apt units MASKED in STEP 15
# =================================================================================================

def _lines_from(text, start_prefix, end_substring):
    lines = text.splitlines()
    s = next(i for i, ln in enumerate(lines) if ln.startswith(start_prefix))
    e = next(i for i in range(s, len(lines)) if end_substring in lines[i])
    return "\n".join(lines[s:e + 1])


def test_setup_device_masks_the_four_apt_units_fail_loud(tmp_path):
    text = SETUP.read_text()
    block = _lines_from(text, "mapfile -t RO_ROOT_APT_UNITS", "|| fail")
    prelude = (f'. "{RO_ROOT}"\nfail() {{ echo "FAIL $*"; exit 1; }}\n'
               'systemctl() { echo "$*" >>"$LOG"; if [ "$1" = mask ]; then return "${MASK_RC:-0}"; fi; return 0; }\n')
    logf = tmp_path / "calls"
    r = _bash("set -euo pipefail\n" + prelude + block, env={"LOG": str(logf)})
    assert r.returncode == 0, r.stderr
    calls = logf.read_text().splitlines()
    assert "mask " + " ".join(APT_UNITS) in calls, calls
    # a mask that fails stops the provisioning by name (never `|| true`)
    logf.unlink()
    r = _bash("set -euo pipefail\n" + prelude + block, env={"LOG": str(logf), "MASK_RC": "1"})
    assert r.returncode != 0 and "FAIL" in r.stdout, (r.stdout, r.stderr)


def test_setup_device_masks_in_step15_before_step16_apt_and_types_no_unit_name():
    text = SETUP.read_text()
    s15 = text.find("# STEP 15:")
    s16 = text.find("# STEP 16:")
    # the pre-flight curl install runs apt-get earlier too; STEP 16's own apt-get is the one after it
    assert 0 <= s15 < text.find("mapfile -t RO_ROOT_APT_UNITS") < s16 < text.find("apt-get update -qq", s16)
    # The names come from the ro-root canon only (one list for setup-device, verify-device, the apply).
    assert "apt-daily" not in _code(text[s15:s16]), _code(text[s15:s16])
    # unattended-upgrades handling is unchanged (#295)
    assert "systemctl mask unattended-upgrades.service" in text[s15:s16]


def test_setup_device_writes_the_logrotate_dropin_before_the_reload_and_the_restore(tmp_path):
    text = SETUP.read_text()
    fstab = text.find("cat > /etc/fstab << FSTABEOF")
    assign = text.find('RO_ROOT_LOGROTATE_DROPIN="/etc/systemd/system/$(ro_root_logrotate_dropin_relpath)"')
    write = text.find('ro_root_logrotate_dropin_content > "$RO_ROOT_LOGROTATE_DROPIN"')
    reload_ = text.find("systemctl daemon-reload", write)
    timer = text.find("systemctl restart logrotate.timer")
    restore = text.find("\nrestore_root_mode\n")
    assert 0 < fstab < assign < write < reload_ < timer < restore, (fstab, assign, write, reload_, timer, restore)
    block = _lines_from(text, 'RO_ROOT_LOGROTATE_DROPIN="', 'ro_root_logrotate_dropin_content >')
    block = block.replace("/etc/systemd/system", str(tmp_path / "etc"))
    r = _bash(f'set -euo pipefail\n. "{RO_ROOT}"\n{block}')
    assert r.returncode == 0, r.stderr
    written = (tmp_path / "etc" / LR_RELPATH).read_text()
    assert written == _lib(RO_ROOT, "ro_root_logrotate_dropin_content")


# =================================================================================================
# 3. the netconsole unit retries; (ak) names a failed netconsole's Result
# =================================================================================================

def _sections(unit):
    out, cur = {}, None
    for ln in unit.splitlines():
        if ln.startswith("[") and ln.endswith("]"):
            cur = ln.strip("[]")
            out[cur] = []
        elif cur and ln and not ln.startswith("#"):
            out[cur].append(ln)
    return out


def test_netconsole_unit_retries_until_dev1_answers():
    unit = _lib(REMOTE_LOG, "remote_log_netconsole_service_unit_content")
    sec = _sections(unit)
    assert "StartLimitIntervalSec=0" in sec["Unit"], sec
    for want in ("Type=oneshot", "RemainAfterExit=yes", "Restart=on-failure", "RestartSec=30"):
        assert want in sec["Service"], (want, sec)


def test_netconsole_unit_is_a_valid_oneshot_for_systemd(tmp_path):
    exe = shutil.which("systemd-analyze")
    assert exe, "systemd-analyze is needed to validate the unit (systemd 255 on dev1 and the CI runner)"
    unit = _lib(REMOTE_LOG, "remote_log_netconsole_service_unit_content")
    unit = re.sub(r"(?m)^ExecStart=.*$", "ExecStart=/bin/true", unit)
    good = tmp_path / "cambox-netconsole.service"
    good.write_text(unit)
    r = subprocess.run([exe, "verify", "--man=no", str(good)], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    # the verifier really checks the oneshot rule: Restart=always is refused for a oneshot
    bad = tmp_path / "bad" / "cambox-netconsole.service"
    bad.parent.mkdir()
    bad.write_text(unit.replace("Restart=on-failure", "Restart=always"))
    r = subprocess.run([exe, "verify", "--man=no", str(bad)], capture_output=True, text=True, timeout=60)
    assert r.returncode != 0, r.stdout + r.stderr


_AK_GREEN = ("NC_SVC_ENABLED=enabled\nNC_SVC_ACTIVE=active\nNC_SVC_RESULT=success\nNC_SCRIPT_X=yes\n"
             "NC_ENABLED=1\nNC_REMOTE_IP=10.77.9.200\nNC_REMOTE_PORT=514\nJU_SVC_ENABLED=enabled\n"
             "JU_URL=http://10.77.9.200:19532\nJU_STATE_SAVE=--save-state=/run/systemd/journal-upload/state")


def test_ak_gathers_and_names_a_failed_netconsole_result():
    snippet = _lib(REMOTE_LOG, "remote_log_gather_remote_snippet")
    assert 'echo "NC_SVC_RESULT=$(systemctl show -p Result --value cambox-netconsole 2>/dev/null)"' in snippet
    assert _lib(REMOTE_LOG, 'remote_log_verdict "$B"', env={"B": _AK_GREEN}).strip() == "ok"
    failed = _AK_GREEN.replace("NC_SVC_ACTIVE=active", "NC_SVC_ACTIVE=failed").replace(
        "NC_SVC_RESULT=success", "NC_SVC_RESULT=exit-code")
    v = _lib(REMOTE_LOG, 'remote_log_verdict "$B"', env={"B": failed})
    assert "cambox-netconsole.service FAILED (Result=exit-code)" in v, v
    retrying = failed.replace("NC_SVC_ACTIVE=failed", "NC_SVC_ACTIVE=activating")
    v = _lib(REMOTE_LOG, 'remote_log_verdict "$B"', env={"B": retrying})
    assert "is not active (state=activating, Result=exit-code)" in v, v
    # an old gather without the Result line still grades (never an unbound read)
    old = failed.replace("NC_SVC_RESULT=exit-code\n", "")
    v = _lib(REMOTE_LOG, 'remote_log_verdict "$B"', env={"B": old})
    assert "FAILED (Result=<unread>)" in v, v


# =================================================================================================
# 4. verify-device (ar): the gather + the verdict (scripts/lib/cambox-ro-units.sh)
# =================================================================================================

def _want():
    lr = _lib(RO_ROOT, "ro_root_logrotate_dropin_content")
    nc = _lib(REMOTE_LOG, "remote_log_netconsole_service_unit_content")
    return lr, nc


def _green(unit_dir="/etc/systemd/system"):
    lr, nc = _want()
    lines = [f"LR_DROPIN_SHA={_sha(lr)}",
             f"LR_DROPIN_LOADED=/usr/lib/systemd/system/logrotate.service.d/x.conf {unit_dir}/{LR_RELPATH}",
             "LR_RESULT=success"]
    lines += [f"APT_UNIT={u} masked inactive" for u in APT_UNITS]
    lines += [f"NC_UNIT_SHA={_sha(nc)}", "NC_RESTART=on-failure"]
    return "\n".join(lines) + "\n"


def _verdict(block, env=None):
    return _lib(UNITS, 'cambox_ro_units_verdict "$B"', env={"B": block, **(env or {})}).strip()


def test_ar_verdict_ok_only_on_the_full_unit_set():
    assert _verdict(_green()) == "ok"


_BAD = [
    ("LR_DROPIN_SHA=", "LR_DROPIN_SHA=__ABSENT__", "logrotate drop-in"),
    ("LR_DROPIN_SHA=", "LR_DROPIN_SHA=" + "0" * 64, "differs from the ro-root canon"),
    ("LR_DROPIN_LOADED=", "LR_DROPIN_LOADED=", "has not loaded"),
    ("LR_RESULT=", "LR_RESULT=exit-code", "Result=exit-code"),
    ("APT_UNIT=apt-daily.timer ", "APT_UNIT=apt-daily.timer enabled active", "apt-daily.timer is enabled, not masked"),
    ("APT_UNIT=apt-daily-upgrade.service ", "APT_UNIT=apt-daily-upgrade.service masked failed",
     "apt-daily-upgrade.service is masked but failed"),
    ("APT_UNIT=apt-daily.service ", "", "no state read for apt-daily.service"),
    ("NC_UNIT_SHA=", "NC_UNIT_SHA=__ABSENT__", "cambox-netconsole.service is missing"),
    ("NC_UNIT_SHA=", "NC_UNIT_SHA=" + "1" * 64, "cambox-netconsole.service differs"),
    ("NC_RESTART=", "NC_RESTART=no", "Restart=no"),
]


@pytest.mark.parametrize("prefix,replacement,needle", _BAD, ids=[b[2] for b in _BAD])
def test_ar_verdict_fails_each_facet_by_name(prefix, replacement, needle):
    lines = _green().splitlines()
    lines = [replacement if ln.startswith(prefix) else ln for ln in lines]
    v = _verdict("\n".join(ln for ln in lines if ln) + "\n")
    assert v != "ok" and needle in v, v
    assert all(ln.startswith("FAIL: ") for ln in v.splitlines()), v


def test_ar_verdict_fails_closed_on_an_empty_read():
    v = _verdict("")
    # one per fact read: the drop-in, the logrotate Result, each apt unit, the netconsole unit (the
    # loaded-checks follow a readable file only)
    assert v.count("FAIL: ") == 3 + len(APT_UNITS), v


# A box-side systemctl that answers the gather's reads from a JSON state (FAKE_SYSTEMD).
_GATHER_SYSTEMCTL = r'''
import json, os, sys
st = json.loads(os.environ["FAKE_SYSTEMD"])
a = sys.argv[1:]
if a[0] == "show":
    prop, unit = a[a.index("-p") + 1], a[-1]
    print(st.get(unit, {}).get(prop, ""))
elif a[0] in ("is-enabled", "is-active"):
    v = st.get(a[1], {}).get(a[0], "")
    if v:
        print(v)
    sys.exit(0 if v in ("enabled", "active") else 1)
'''


def _gather(tmp_path, state, files):
    unit_dir = tmp_path / "etc-systemd"
    for rel, body in files.items():
        p = unit_dir / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body)
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    (bindir / "systemctl").write_text(f"#!{sys.executable}\n{_GATHER_SYSTEMCTL}")
    (bindir / "systemctl").chmod(0o755)
    env = {"CAMBOX_RO_UNITS_UNIT_DIR": str(unit_dir)}
    snippet = _lib(UNITS, "cambox_ro_units_gather_remote_snippet", env=env)
    r = _bash(snippet, env={"PATH": f"{bindir}:/usr/bin:/bin", "FAKE_SYSTEMD": json.dumps(state)})
    assert r.returncode == 0, r.stderr
    return r.stdout, env, unit_dir


def test_ar_gather_reads_a_applied_box_as_ok_and_a_stock_box_as_failed(tmp_path):
    lr, nc = _want()
    dropin = tmp_path / "etc-systemd" / LR_RELPATH
    state = {"logrotate.service": {"DropInPaths": f"{dropin}", "Result": "success"},
             "cambox-netconsole.service": {"Restart": "on-failure"}}
    for u in APT_UNITS:
        state[u] = {"is-enabled": "masked", "is-active": "inactive"}
    block, env, _ = _gather(tmp_path, state, {LR_RELPATH: lr, "cambox-netconsole.service": nc})
    assert _verdict(block, env) == "ok", block
    # the live finding: no drop-in, logrotate failed, the apt units only disabled, the old unit
    stock = {"logrotate.service": {"DropInPaths": "", "Result": "exit-code"},
             "cambox-netconsole.service": {"Restart": "no"}}
    for u in APT_UNITS:
        stock[u] = {"is-enabled": "disabled", "is-active": "failed" if u.endswith(".service") else "active"}
    old_unit = nc.replace("Restart=on-failure\n", "").replace("RestartSec=30\n", "")
    block, env, _ = _gather(tmp_path / "stock", stock, {"cambox-netconsole.service": old_unit})
    v = _verdict(block, env)
    assert "logrotate drop-in" in v and "is missing" in v, v
    assert "apt-daily.timer is disabled, not masked" in v, v
    assert "cambox-netconsole.service differs" in v, v


# =================================================================================================
# 5. verify-device.sh wiring of (ar)
# =================================================================================================

def _ar_block(text):
    ar = text.find("\n# (ar) ")
    al = text.find("\n# (al) ", ar)
    assert 0 <= ar < al, "(ar) sits right before (al)"
    return text[ar:al]


def test_verify_device_documents_sources_and_places_ar():
    text = VERIFY.read_text()
    header = text[:text.find("set -euo pipefail")]
    assert "(ar)" in header and "issue 1394" in header[header.find("(ar)"):]
    usage = text[text.find("usage() {"):]
    assert "(ar)" in usage[:usage.find("\nEOF\n")]
    assert re.search(r'(?m)^\. "\$HERE/lib/cambox-ro-units\.sh"', text)
    guard = text.find("never run the live SSH flow below.")
    ar = text.find("\n# (ar) ")
    q = text.rfind("# (q) .bak cruft drift")
    assert guard < text.find("\n# (ak) ") < ar < text.find("\n# (al) ") < q
    # never inside the slices other tests EXECUTE: (aq)..(ao), (ao)..(an), (an)..(q)
    assert not (text.find("\n# (aq) ") < ar < q)


def test_verify_device_ar_is_a_hard_gate_over_the_shared_lib():
    code = _code(_ar_block(VERIFY.read_text()))
    assert "cambox_ro_units_gather_remote_snippet" in code and "cambox_ro_units_verdict" in code
    assert 'fail "' in code and 'warn "' not in code


def _run_ar(ssh_out, ssh_rc=0):
    block = _ar_block(VERIFY.read_text())
    prelude = (f'. "{UNITS}"\n'
               'ok() { echo "OK $1"; }\nfail() { echo "FAIL $1"; }\n'
               f'ssh_box() {{ printf "%s" "$SSH_OUT"; return {ssh_rc}; }}\n')
    r = _bash("set -euo pipefail\n" + prelude + block, env={"SSH_OUT": ssh_out, "CAMERA_NAME": "cam1"})
    assert r.returncode == 0, r.stdout + r.stderr
    return r.stdout


def test_verify_device_ar_runs_green_red_and_unreachable():
    out = _run_ar(_green())
    assert out.startswith("OK ") and "FAIL" not in out, out
    out = _run_ar(_green().replace("LR_RESULT=success", "LR_RESULT=exit-code"))
    assert "FAIL" in out and "Result=exit-code" in out and "cambox-ro-units-apply.sh" in out, out
    out = _run_ar("", ssh_rc=255)
    assert out.startswith("FAIL ") and "rc=255" in out, out


# =================================================================================================
# 6. the live-apply program on the fake read-only-root box
# =================================================================================================

# mv as the program's only rename: logged with the fake fs prefix stripped, and refusing a write
# under the box fs while the root reads ro (the deploy-fleet harness pattern).
_LOGGED_MV = r'''
import os, sys
real = __REAL__
st = os.environ["FAKE_STATE"]
fs = os.path.join(st, "fs")
args = [a.replace(fs, "") for a in sys.argv[1:]]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write("mv " + " ".join(args) + f" root={root}\n")
if root != "rw" and any(a.startswith(fs) for a in sys.argv[1:]):
    sys.stderr.write("mv: cannot move: Read-only file system\n")
    sys.exit(1)
if os.environ.get("FAKE_MV_RC"):  # a rename that fails while the window is open
    sys.stderr.write("mv: forced failure\n")
    sys.exit(int(os.environ["FAKE_MV_RC"]))
os.execv(real, [real] + sys.argv[1:])
'''

_FAILED_LIVE = ["logrotate.service", "apt-daily.service", "apt-daily-upgrade.service", "cambox-netconsole.service"]


def _box(tmp_path, root_mode="ro", nc_present=True):
    box = make_box(tmp_path, root=root_mode)
    stub = box["stub"]
    for tool in ("mkdir", "hostname"):
        stub.joinpath(tool).symlink_to(shutil.which(tool, path="/usr/bin:/bin"))
    stub.joinpath("mv").unlink()
    stub.joinpath("mv").write_text(f"#!{sys.executable}\n" + _LOGGED_MV.replace("__REAL__", repr(shutil.which("mv", path="/usr/bin:/bin"))))
    stub.joinpath("mv").chmod(0o755)
    unit_dir = box["state"] / "fs" / "etc" / "systemd" / "system"
    unit_dir.mkdir(parents=True)
    _lr, nc = _want()
    if nc_present:  # the pre-1394 unit: no Restart=, no start-limit lift
        unit_dir.joinpath("cambox-netconsole.service").write_text(
            nc.replace("Restart=on-failure\n", "").replace("RestartSec=30\n", "").replace("StartLimitIntervalSec=0\n", ""))
    for u in _FAILED_LIVE:  # the live finding: all four units failed
        box["state"].joinpath(f"failed-{u}").write_text("failed\n")
    return box, unit_dir


def _program(unit_dir):
    return build(f'. "{UNITS}"\ncambox_ro_units_apply_program', env={"CAMBOX_RO_UNITS_UNIT_DIR": str(unit_dir)})


def test_apply_writes_inside_one_verified_window_and_starts_only_after_it(tmp_path):
    box, unit_dir = _box(tmp_path)
    proc = run_text(box, _program(unit_dir), strict="set -euo pipefail")
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    lr, nc = _want()
    assert unit_dir.joinpath(LR_RELPATH).read_text() == lr
    assert unit_dir.joinpath("cambox-netconsole.service").read_text() == nc
    rw = index(calls, "mount -o remount,rw / root=ro")
    mvs = [i for i, c in enumerate(calls) if c.startswith("mv ")]
    mask = index(calls, "systemctl mask --no-reload " + " ".join(APT_UNITS) + " root=rw")
    close = index(calls, "mount -o remount,ro / root=rw")
    assert len(mvs) == 2 and all(rw < i < close for i in mvs + [mask]), "\n".join(calls)
    assert all(calls[i].endswith("root=rw") for i in mvs), "\n".join(calls)
    # round 2: the apt units are stopped BEFORE the window, so no apt timer fires inside it
    assert index(calls, "systemctl stop " + " ".join(APT_UNITS) + " root=ro") < rw, "\n".join(calls)
    after = [index(calls, p, close) for p in (
        "systemctl daemon-reload root=ro",
        "systemctl reset-failed logrotate.service root=ro",
        "systemctl start logrotate.service root=ro",
        "systemctl restart cambox-netconsole.service root=ro",
        "systemctl is-system-running root=ro")]
    assert after == sorted(after), "\n".join(calls)
    for u in _FAILED_LIVE:
        index(calls, f"systemctl reset-failed {u} root=ro", close)
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert sum(c.startswith("mount -o remount,ro") for c in calls) == 1, "one close, never a retry"
    assert root(box) == "ro"
    assert "is-system-running = running" in proc.stdout, proc.stdout


def test_apply_on_a_busy_root_fails_loud_and_starts_nothing(tmp_path):
    box, unit_dir = _box(tmp_path)
    proc = run_text(box, _program(unit_dir), FAKE_RO_FAIL="1")
    calls = log(box)
    assert proc.returncode != 0
    assert "root is NOT read-only" in proc.stderr and "systemd-journal" in proc.stderr, proc.stderr
    close = index(calls, "mount -o remount,ro")
    assert not any(c.startswith("systemctl") for c in calls[close:]), "\n".join(calls)
    assert sum(c.startswith("mount -o remount,ro") for c in calls) == 1


def test_apply_on_a_refused_rw_remount_writes_nothing(tmp_path):
    box, unit_dir = _box(tmp_path)
    proc = run_text(box, _program(unit_dir), FAKE_RW_FAIL="1")
    calls = log(box)
    assert proc.returncode != 0
    assert not unit_dir.joinpath(LR_RELPATH).exists()
    assert not any(c.startswith(("mv ", "systemctl mask", "systemctl start", "systemctl restart")) for c in calls), calls


def test_apply_is_idempotent_and_opens_no_window_when_everything_is_in_place(tmp_path):
    box, unit_dir = _box(tmp_path)
    first = run_text(box, _program(unit_dir))
    assert first.returncode == 0, first.stderr
    box["state"].joinpath("log").write_text("")
    again = run_text(box, _program(unit_dir))
    calls = log(box)
    assert again.returncode == 0, again.stderr
    assert not any(c.startswith("mount ") for c in calls), "\n".join(calls)
    index(calls, "systemctl daemon-reload")
    index(calls, "systemctl start logrotate.service")


def test_apply_on_a_stuck_writable_root_forces_it_back_read_only(tmp_path):
    box, unit_dir = _box(tmp_path, root_mode="rw")
    proc = run_text(box, _program(unit_dir))
    assert proc.returncode == 0, proc.stderr
    assert root(box) == "ro"
    assert starts_on_rw(log(box)) == []


def test_apply_without_the_1311_netconsole_unit_writes_none_and_restarts_none(tmp_path):
    box, unit_dir = _box(tmp_path, nc_present=False)
    proc = run_text(box, _program(unit_dir))
    calls = log(box)
    assert proc.returncode == 0, proc.stderr
    assert not unit_dir.joinpath("cambox-netconsole.service").exists()
    assert not any("cambox-netconsole" in c and ("restart" in c or c.startswith("mv ")) for c in calls), calls
    assert "setup-device.sh" in proc.stdout and "issue 1311" in proc.stdout, proc.stdout


def test_apply_reports_a_box_that_stays_degraded(tmp_path):
    box, unit_dir = _box(tmp_path)
    proc = run_text(box, _program(unit_dir), FAKE_SYSTEM_STATE="degraded")
    assert proc.returncode != 0
    assert "is-system-running = degraded" in proc.stderr, proc.stderr


def test_apply_names_a_logrotate_run_that_still_fails(tmp_path):
    box, unit_dir = _box(tmp_path)
    proc = run_text(box, _program(unit_dir), FAKE_START_RC="1")
    assert proc.returncode != 0
    assert "logrotate.service" in proc.stderr and "FAIL" in proc.stderr, proc.stderr
    assert root(box) == "ro"


def test_apply_program_keeps_its_commands_off_the_programs_stdin():
    # `ssh ... bash -s` feeds the program on stdin: a command that read stdin would eat the rest of
    # it, the verified close included (the rt-kernel-plan finding). Everything runs inside one
    # function called with </dev/null.
    text = _program("/etc/systemd/system")
    assert text.rstrip().endswith("_rou_main </dev/null"), text[-200:]
    assert re.search(r"(?m)^_rou_main\(\) \{$", text)


# =================================================================================================
# 7. the CLI: --plan touches nothing, --apply runs each box, boxes come from camera-set.sh
# =================================================================================================

_SSHPASS = r'''
import os, sys
d = os.environ["FAKE_SSH_DIR"]
n = len([f for f in os.listdir(d) if f.startswith("argv-")])
open(os.path.join(d, f"argv-{n}"), "w").write("\n".join(sys.argv[1:]))
open(os.path.join(d, f"stdin-{n}"), "w").write(sys.stdin.read())
host = next(a for a in sys.argv if a.startswith("root@"))
fail = os.environ.get("FAKE_FAIL_HOST", "")
if host == "root@" + os.environ.get("FAKE_TAKE_LEASE_ON_HOST", ""):  # an E2E takes the rig mid-run
    d = os.environ["RIG_LEASE_DIR"]
    os.makedirs(d, exist_ok=True)
    open(os.path.join(d, "holder.json"), "w").write(
        '{"repo": "zbynekdrlik/camera-box", "run_id": "4242", "run_url": "u", "job": "e2e"}')
    open(os.path.join(d, "heartbeat"), "w").write("")
if host == "root@" + os.environ.get("FAKE_RUNTIME_FAIL_HOST", ""):  # files land, the arm fails
    print("OK: the unit files are written and the root of cam1 reads read-only again (ro,relatime)")
    sys.stderr.write("FAIL: [issue 1394] cambox-netconsole.service did not arm on cam1 (Result=exit-code); it retries now\n")
    sys.exit(1)
if host == "root@" + os.environ.get("FAKE_NO_NC_HOST", ""):
    print("NOTE: no /etc/systemd/system/cambox-netconsole.service on cam3 -- issue 1311 netconsole is not provisioned here; re-run setup-device.sh for it (this program writes no new unit)")
if host == "root@" + fail:
    if os.environ.get("FAKE_CLOSE_FAIL"):
        sys.stderr.write("FAIL: [issue 1394] cam1's root is NOT read-only after the remount-rw window ('findmnt -no OPTIONS /' = 'rw' -> rw; the ro remount rc=32: busy; sync rc=0). x\n")
        sys.stderr.write("FAIL: [issue 1394] processes with a file open for WRITING on / ('fuser -vm /', ACCESS F):\n")
        sys.stderr.write("                     root      76355 F.... systemd-journal\n")
        sys.stderr.write("FAIL: [issue 1394] hint\n")
    sys.exit(1)
print("OK: applied on " + host)
'''


# The rig-busy read (obs_phase2.py rig-busy-check) the --apply guard makes, answered from FAKE_BUSY
# and logged, so a test never talks to the real strih / stream OBS.
_OBS_PHASE2 = r'''
import json, os, sys
open(os.environ["FAKE_OBS_LOG"], "a").write(" ".join(sys.argv[1:]) + "\n")
if sys.argv[1:2] == ["rig-busy-check"]:
    busy = os.environ.get("FAKE_BUSY") == "1"
    print(json.dumps({"busy": busy, "hint": "fake",
                      "diagnostics": [{"host": "stream", "streaming": busy, "recording": False}]}))
'''


def _cli(tmp_path, *args, env=None):
    bindir = tmp_path / "dev1-bin"
    bindir.mkdir(exist_ok=True)
    (bindir / "sshpass").write_text(f"#!{sys.executable}\n{_SSHPASS}")
    (bindir / "sshpass").chmod(0o755)
    sshdir = tmp_path / "ssh"
    sshdir.mkdir(exist_ok=True)
    obsdir = tmp_path / "obs"
    obsdir.mkdir(exist_ok=True)
    (obsdir / "obs_phase2.py").write_text(_OBS_PHASE2)
    # Every rig input of the --apply guard points into tmp_path: no real lease dir, no real
    # issue-281 heartbeat, no real OBS (the CI runner has no rig network; dev1 has a live one).
    e = {"PATH": f"{bindir}:/usr/bin:/bin", "FAKE_SSH_DIR": str(sshdir), "HOME": str(tmp_path),
         "RIG_LEASE_DIR": str(tmp_path / "rig-lease"), "CAMERA_BOX_RIG_HEARTBEAT": str(tmp_path / "rig-active"),
         "CAMBOX_RO_UNITS_OBS_PHASE2_DIR": str(obsdir), "FAKE_OBS_LOG": str(tmp_path / "obs.log")}
    e.update(env or {})
    r = subprocess.run(["bash", str(CLI), *args], capture_output=True, text=True, env=e, timeout=120)
    return r, sshdir


def test_cli_plan_prints_the_program_and_touches_nothing(tmp_path):
    r, sshdir = _cli(tmp_path, "--plan", "--box", "cam1")
    assert r.returncode == 0, r.stderr
    assert "== cam1 (10.77.9.61)" in r.stdout
    assert _program("/etc/systemd/system").strip() in r.stdout
    assert os.listdir(sshdir) == [], "--plan never connects"


def test_cli_apply_feeds_the_program_to_bash_on_the_box(tmp_path):
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam1")
    assert r.returncode == 0, r.stdout + r.stderr
    argv = (sshdir / "argv-0").read_text().splitlines()
    assert "root@10.77.9.61" in argv and argv[-2:] == ["bash", "-s"], argv
    assert "UserKnownHostsFile=/dev/null" in argv
    assert (sshdir / "stdin-0").read_text().strip() == _program("/etc/systemd/system").strip()


def test_cli_apply_runs_every_box_and_names_the_failed_ones(tmp_path):
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam1", "--box", "cam3",
                     env={"FAKE_FAIL_HOST": "10.77.9.61", "FAKE_CLOSE_FAIL": "1"})
    assert r.returncode == 1, r.stdout + r.stderr
    assert len([f for f in os.listdir(sshdir) if f.startswith("argv-")]) == 2, "a failed box never stops the rest"
    summary = r.stdout + r.stderr
    assert "cam1" in summary and "root-rw" in summary and "systemd-journal[76355]" in summary, summary


def test_cli_active_takes_the_boxes_from_camera_active_set(tmp_path):
    r, _ = _cli(tmp_path, "--plan", "--active", env={"CAMERA_ACTIVE_SET": "cam3 cam5"})
    assert r.returncode == 0, r.stderr
    assert re.findall(r"(?m)^== (cam\d) ", r.stdout) == ["cam3", "cam5"], r.stdout


@pytest.mark.parametrize("args", [(), ("--plan",), ("--box", "cam1"), ("--plan", "--apply", "--box", "cam1"),
                                  ("--plan", "--box", "cam99"), ("--bogus",)])
def test_cli_refuses_a_bad_invocation(tmp_path, args):
    r, sshdir = _cli(tmp_path, *args)
    assert r.returncode == 2, (args, r.returncode, r.stdout, r.stderr)
    assert os.listdir(sshdir) == []


def test_cli_and_lib_never_type_a_camera_range():
    for p in (CLI, UNITS):
        code = _code(p.read_text())
        assert not re.search(r"\bcam[1-9]\b", code), p
        assert not re.search(r"for \w+ in 1 2 3", code), p


def test_the_new_scripts_parse_and_lint():
    for p in (UNITS, CLI, RO_ROOT, SBC, REMOTE_LOG):
        assert subprocess.run(["bash", "-n", str(p)]).returncode == 0, p
    sc = shutil.which("shellcheck")
    assert sc, "shellcheck is preinstalled on dev1 and the CI runner"
    r = subprocess.run([sc, "-S", "warning", str(UNITS), str(CLI)], capture_output=True, text=True)
    assert r.returncode == 0, r.stdout


# =================================================================================================
# 8. review round 1: a stuck-writable root, a failure inside the window, the rig guard, backoff
# =================================================================================================

def test_apply_on_a_stuck_writable_root_with_nothing_to_write_closes_before_any_start(tmp_path):
    # Review finding 1: with every file and mask already in place, the window (and its verified
    # close) used to be skipped, so a root stuck WRITABLE got logrotate + netconsole started on it.
    box, unit_dir = _box(tmp_path)
    assert run_text(box, _program(unit_dir)).returncode == 0
    box["state"].joinpath("root").write_text("rw\n")
    box["state"].joinpath("log").write_text("")
    proc = run_text(box, _program(unit_dir))
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert root(box) == "ro"
    assert sum(c.startswith("mount -o remount,ro") for c in calls) == 1, "\n".join(calls)
    assert not any(c.startswith(("mv ", "systemctl mask")) for c in calls), "nothing to write"
    assert "reads 'rw'" in proc.stdout, proc.stdout


def test_apply_on_a_stuck_writable_busy_root_fails_loud_and_starts_nothing(tmp_path):
    box, unit_dir = _box(tmp_path)
    assert run_text(box, _program(unit_dir)).returncode == 0
    box["state"].joinpath("root").write_text("rw\n")
    box["state"].joinpath("log").write_text("")
    proc = run_text(box, _program(unit_dir), FAKE_RO_FAIL="1")
    calls = log(box)
    assert proc.returncode != 0
    assert "root is NOT read-only" in proc.stderr, proc.stderr
    assert not any(c.startswith(("systemctl start", "systemctl restart", "systemctl daemon-reload")) for c in calls), calls


def test_apply_failure_inside_the_window_closes_it_and_starts_nothing(tmp_path):
    # Review finding 2: the EXIT trap is what closes the window when a write fails inside it.
    box, unit_dir = _box(tmp_path)
    proc = run_text(box, _program(unit_dir), FAKE_MV_RC="1")
    calls = log(box)
    assert proc.returncode != 0
    assert root(box) == "ro", "\n".join(calls)
    assert sum(c.startswith("mount -o remount,ro") for c in calls) == 1, "\n".join(calls)
    assert not any(c.startswith(("systemctl start", "systemctl restart", "systemctl daemon-reload")) for c in calls), calls
    assert "could not write" in proc.stderr, proc.stderr
    assert not list(unit_dir.rglob("*.new")), "a failed write leaves no temp file behind"


def test_apply_names_a_refused_rw_remount(tmp_path):
    box, unit_dir = _box(tmp_path)
    proc = run_text(box, _program(unit_dir), FAKE_RW_FAIL="1")
    assert proc.returncode != 0
    assert "could not remount / read-write" in proc.stderr and "nothing was written" in proc.stderr, proc.stderr


def test_netconsole_retry_backs_off_to_ten_minutes():
    # Review nit: a box away from dev1 (the travelling rig) retried every 30 s forever into the
    # issue-1309 journal on the USB stick. systemd 255 RestartSteps/RestartMaxDelaySec (validated by
    # the systemd-analyze test above, which verifies this same unit text).
    sec = _sections(_lib(REMOTE_LOG, "remote_log_netconsole_service_unit_content"))
    assert "RestartSteps=4" in sec["Service"] and "RestartMaxDelaySec=600" in sec["Service"], sec


def test_ar_verdict_points_runtime_failures_at_the_apply_not_at_setup_device():
    # Review nit: setup-device never clears a unit's failed state (and a cambox is never rebooted
    # remotely), so for a failed unit only the apply script is a fix.
    lines = [ln if not ln.startswith("APT_UNIT=apt-daily.service ") else "APT_UNIT=apt-daily.service masked failed"
             for ln in _green().splitlines()]
    v = _verdict("\n".join(lines) + "\n")
    assert "cambox-ro-units-apply.sh" in v and "setup-device" not in v, v
    v = _verdict(_green().replace("LR_RESULT=success", "LR_RESULT=exit-code"))
    assert "cambox-ro-units-apply.sh" in v and "setup-device" not in v, v
    nc_missing = "\n".join("NC_UNIT_SHA=__ABSENT__" if ln.startswith("NC_UNIT_SHA=") else ln
                           for ln in _green().splitlines()) + "\n"
    v = _verdict(nc_missing)
    assert "setup-device.sh" in v, v


def _seed_lease(tmp_path):
    d = tmp_path / "rig-lease"
    d.mkdir()
    (d / "holder.json").write_text(json.dumps({"repo": "zbynekdrlik/camera-box", "run_id": "4242",
                                               "run_url": "https://example.invalid/4242", "job": "e2e",
                                               "acquired_at": "2026-10-08T10:00:00Z",
                                               "expected_release_at": "2026-10-08T11:00:00Z"}))
    (d / "heartbeat").write_text("")


def _obs_calls(tmp_path):
    f = tmp_path / "obs.log"
    return f.read_text().splitlines() if f.exists() else []


def test_cli_apply_refuses_while_a_live_rig_lease_is_held(tmp_path):
    # Review finding 3: no cambox root write while an E2E holds the rig (the 17.9.2026 incident).
    _seed_lease(tmp_path)
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam1")
    assert r.returncode == 1, r.stdout + r.stderr
    assert os.listdir(sshdir) == [], "no box touched"
    assert "zbynekdrlik/camera-box#4242" in r.stderr, r.stderr


def test_cli_apply_refuses_while_the_issue_281_rig_heartbeat_is_fresh(tmp_path):
    (tmp_path / "rig-active").write_text("%d\trecording-e2e\t4242\n" % int(time.time()))
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam1")
    assert r.returncode == 1, r.stdout + r.stderr
    assert os.listdir(sshdir) == [], "no box touched"
    assert "heartbeat" in r.stderr, r.stderr


def test_cli_apply_refuses_while_strih_or_stream_broadcasts(tmp_path):
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam1", env={"FAKE_BUSY": "1"})
    assert r.returncode == 1, r.stdout + r.stderr
    assert os.listdir(sshdir) == [], "no box touched"
    assert any(c.startswith("rig-busy-check") for c in _obs_calls(tmp_path))


def test_cli_apply_force_live_bypasses_the_rig_guard_loudly(tmp_path):
    _seed_lease(tmp_path)
    r, sshdir = _cli(tmp_path, "--apply", "--force-live", "--box", "cam1", env={"FAKE_BUSY": "1"})
    assert r.returncode == 0, r.stdout + r.stderr
    assert "--force-live" in r.stderr and "WARNING" in r.stderr, r.stderr
    assert len([f for f in os.listdir(sshdir) if f.startswith("argv-")]) == 1


def test_cli_plan_never_runs_the_rig_guard(tmp_path):
    _seed_lease(tmp_path)
    r, _ = _cli(tmp_path, "--plan", "--box", "cam1", env={"FAKE_BUSY": "1"})
    assert r.returncode == 0, r.stderr
    assert _obs_calls(tmp_path) == []


def test_cli_apply_keeps_a_dead_connection_from_hanging_the_fleet_loop(tmp_path):
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam1")
    assert r.returncode == 0, r.stdout + r.stderr
    argv = (sshdir / "argv-0").read_text().splitlines()
    assert "ServerAliveInterval=10" in argv and "ServerAliveCountMax=6" in argv, argv


def test_cli_names_a_box_without_the_1311_netconsole_unit_in_its_result(tmp_path):
    r, _ = _cli(tmp_path, "--apply", "--box", "cam1", "--box", "cam3", env={"FAKE_NO_NC_HOST": "10.77.9.63"})
    assert r.returncode == 0, r.stdout + r.stderr
    assert "no issue-1311 netconsole unit on: cam3" in r.stdout + r.stderr, r.stdout + r.stderr


_CAMBOX_PROVISIONERS = ["scripts/setup-device.sh", "scripts/build-image.sh", "scripts/create-usb-linux.sh",
                        "scripts/verify-device.sh", "scripts/lib/cambox-ro-units.sh",
                        "scripts/cambox-ro-units-apply.sh"]


def test_no_cambox_provisioner_types_an_apt_unit_name():
    # The four names live ONLY in ro_root_masked_apt_units: setup-device, the overlay image builder,
    # verify-device and the live apply all read that one list.
    for rel in _CAMBOX_PROVISIONERS:
        code = _code((REPO / rel).read_text())
        for unit in APT_UNITS:
            assert unit not in code, (rel, unit)
    assert "ro_root_masked_apt_units" in _code((REPO / "scripts" / "build-image.sh").read_text())


# =================================================================================================
# 9. review round 2: the guard before EVERY box, the arm-failure exit, named post-close failures
# =================================================================================================

def test_cli_apply_reads_the_rig_guard_before_every_box(tmp_path):
    # Review round 2 finding 1: an E2E that takes the lease while cam1 is applied must stop the run
    # before cam3 is touched (one box can take minutes: the netconsole arm waits for dev1).
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam1", "--box", "cam3",
                     env={"FAKE_TAKE_LEASE_ON_HOST": "10.77.9.61"})
    assert r.returncode == 1, r.stdout + r.stderr
    assert len([f for f in os.listdir(sshdir) if f.startswith("argv-")]) == 1, "cam3 must stay untouched"
    assert "Not touched: cam3" in r.stderr and "#4242" in r.stderr, r.stderr


def test_cli_apply_goes_ahead_on_a_stale_or_broken_lease(tmp_path):
    # Review round 2 nit 4: only a LIVE holder refuses; a stale heartbeat or a lockdir without
    # holder.json is reclaimable (rig_lease_is_stale), never a reason to refuse.
    _seed_lease(tmp_path)
    old = time.time() - 7200
    os.utime(tmp_path / "rig-lease" / "heartbeat", (old, old))
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam1", env={"RIG_LEASE_STALE_SECS": "3600"})
    assert r.returncode == 0, r.stdout + r.stderr
    assert len([f for f in os.listdir(sshdir) if f.startswith("argv-")]) == 1
    # a lockdir with a fresh heartbeat but no holder.json (re-seeded: the run above may have taken
    # and released the lease)
    lease = tmp_path / "rig-lease"
    lease.mkdir(exist_ok=True)
    (lease / "holder.json").unlink(missing_ok=True)
    (lease / "heartbeat").write_text("")
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam1")
    assert r.returncode == 0, r.stdout + r.stderr


def test_the_rig_held_read_is_one_shared_function():
    # Review round 2 nit 5: the "is the rig driven right now" read (issue-281 heartbeat OR a live
    # issue-830 lease) lives ONCE in rig-heartbeat.sh; the apply and the burn-reconcile watchdog
    # both call it.
    lib = (LIB / "rig-heartbeat.sh").read_text()
    body = lib[lib.index("rig_held_reason() {"):]
    body = body[:body.index("\n}\n")]
    assert "rig_heartbeat_active" in body and "rig_lease_is_stale" in body, body
    for rel in ("scripts/cambox-ro-units-apply.sh", "scripts/obs-burn-reconcile-watchdog.sh"):
        code = _code((REPO / rel).read_text())
        assert "rig_held_reason" in code, rel
        assert "rig_heartbeat_active" not in code and "rig_lease_is_stale" not in code, rel


def test_apply_fails_a_box_whose_netconsole_cannot_arm(tmp_path):
    # Review round 2 finding 2: on systemd 255 a Restart=on-failure oneshot whose restart fails sits
    # in `activating (auto-restart)`, not `failed`, so is-system-running stays `running`; only the
    # program's own exit code can fail the box.
    box, unit_dir = _box(tmp_path)
    proc = run_text(box, _program(unit_dir), FAKE_RESTART_FAIL_UNIT="cambox-netconsole.service")
    assert proc.returncode != 0, proc.stdout + proc.stderr
    assert "did not arm" in proc.stderr, proc.stderr
    assert "is-system-running = running" in proc.stdout, proc.stdout
    assert root(box) == "ro"


def test_netconsole_unit_has_no_key_systemd_would_ignore(tmp_path):
    # Review round 2 nit 3: systemd-analyze verify exits 0 on an unknown key or a bad value, only
    # warning "... ignoring"; the good unit must print no such warning.
    exe = shutil.which("systemd-analyze")
    assert exe, "systemd-analyze is needed (systemd 255 on dev1 and the CI runner)"
    unit = _lib(REMOTE_LOG, "remote_log_netconsole_service_unit_content")
    unit = re.sub(r"(?m)^ExecStart=.*$", "ExecStart=/bin/true", unit)
    good = tmp_path / "cambox-netconsole.service"
    good.write_text(unit)
    r = subprocess.run([exe, "verify", "--man=no", str(good)], capture_output=True, text=True, timeout=60)
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "ignoring" not in out.lower() and "unknown key" not in out.lower(), out
    # the check bites: a misspelled key is reported as ignored
    bad = tmp_path / "bad" / "cambox-netconsole.service"
    bad.parent.mkdir()
    bad.write_text(unit.replace("RestartSteps=", "RestartStep="))
    r = subprocess.run([exe, "verify", "--man=no", str(bad)], capture_output=True, text=True, timeout=60)
    assert "ignoring" in (r.stdout + r.stderr).lower(), r.stdout + r.stderr


def test_apply_names_a_failed_daemon_reload_and_still_reads_back(tmp_path):
    box, unit_dir = _box(tmp_path)
    proc = run_text(box, _program(unit_dir), FAKE_DAEMON_RELOAD_RC="1")
    calls = log(box)
    assert proc.returncode != 0
    assert "FAIL: [issue 1394] systemctl daemon-reload failed" in proc.stderr, proc.stderr
    index(calls, "systemctl is-system-running")


def test_apply_names_a_failed_mkdir_inside_the_window(tmp_path):
    box, unit_dir = _box(tmp_path)
    unit_dir.joinpath("logrotate.service.d").write_text("not a directory\n")
    proc = run_text(box, _program(unit_dir))
    calls = log(box)
    assert proc.returncode != 0
    assert "could not write" in proc.stderr, proc.stderr
    assert root(box) == "ro", "\n".join(calls)
    assert not any(c.startswith(("systemctl start", "systemctl restart")) for c in calls), calls


def test_cli_result_tells_landed_files_from_a_failed_check_after_the_close(tmp_path):
    # Review round 2 nit 6: "did NOT land" was printed even when the files landed and only a check
    # after the close (the netconsole arm, a failed unit) failed.
    r, _ = _cli(tmp_path, "--apply", "--box", "cam1", env={"FAKE_RUNTIME_FAIL_HOST": "10.77.9.61"})
    assert r.returncode == 1, r.stdout + r.stderr
    assert "cam1 (the unit files are in place; a step outside the rw window failed: cambox-netconsole.service did not arm" \
        in r.stderr, r.stderr
