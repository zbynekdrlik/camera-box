"""Issue 1485, dstack v0.6.28 (owner ruling relayed by dstack#1, 4.11.2026): no CPU-core pinning for dlink apps.

camera-box's Dante CPU reservation is gone (the root helper strih-dante-rx-pin, its sudoers line and conf,
OBS's reduced mask, the hub ExecStartPost drop-in, the systemd + user-manager CPUAffinity drop-ins, the
JPEG XS cores derived from it). setup-strih step 13 REMOVES what an earlier run left on a box, on every box
(`strih_dante_cpu_leftovers_remove`), re-executes PID 1 + the operator's user manager only when a manager
drop-in went, reloads only when the hub drop-in went, and leaves a file of the same name without our
header alone. verify item 9 FAILs every leftover (`strih_dante_cpu_leftovers_grade`).
Every bash run sources the libs under the caller's `set -euo pipefail`; values reach bash as arguments.
"""
import json
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_NIC_IFACE", "STRIH_BOXES_DIR", "OBS_FLEET", "GH_TOKEN",
         "STRIH_DANTE_INPUT_SITE_FILE", "STRIH_DANTE_INPUT_TOML", "STRIH_DANTE_INPUT_IFACE",
         "STRIH_DANTE_RX_PIN_ROOT", "STRIH_DANTE_RX_PIN_OWNER", "OBS_BOX_ONLINE_CPUS")

MANAGER_HEADER = "# issue 1485: written by camera-box setup-strih step 13 on a Dante-program box; every run rewrites it."
HUB_HEADER = ("# issue 1485: written by camera-box setup-strih step 13 on a box whose hub Dante device has its own CPU;"
              " every run rewrites it.")
# what the removed libs wrote, as strih-snv carries it (read live 4.11.2026)
CONF = ("# issue 1485: written by setup-strih step 13 on a Dante-program box; every run rewrites it.\n"
        "reserved=15\nobs=2-14\nbase=2,3,4,5,6,7,8,9,10,11,12,13,14,15\nsmt_siblings=14-15\n")
MANAGER = MANAGER_HEADER + "\n[Manager]\nCPUAffinity=0-14\n"
HUB = HUB_HEADER + "\n[Service]\nExecStartPost=-+/usr/local/sbin/strih-dante-rx-pin hub\n"

PATHS = {
    "conf": "etc/strih-dante-rx-cpu.conf",
    "helper": "usr/local/sbin/strih-dante-rx-pin",
    "sudoers": "etc/sudoers.d/strih-dante-rx-pin",
    "system": "etc/systemd/system.conf.d/50-strih-dante-reserved-cpu.conf",
    "user": "etc/systemd/user.conf.d/50-strih-dante-reserved-cpu.conf",
    "hub": "etc/systemd/system/intercom-hub.service.d/60-strih-dante-rx-pin.conf",
}
TEXT = {"conf": CONF, "helper": "#!/bin/bash\nexit 0\n", "sudoers": "newlevel ALL=(root) NOPASSWD: x\n",
        "system": MANAGER, "user": MANAGER, "hub": HUB}


def _user():
    return subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()


def _box(tmp, which=tuple(PATHS)):
    root = tmp / "root"
    for k in which:
        f = root / PATHS[k]
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(TEXT[k])
    (root / "etc").mkdir(parents=True, exist_ok=True)
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    log = tmp / "systemctl.log"
    log.write_text("")
    f = b / "systemctl"
    f.write_text('#!/bin/bash\nprintf "%%s\\n" "$*" >> "%s"\n' % log)
    f.chmod(0o755)
    env = {"STRIH_DANTE_RX_PIN_ROOT": str(root), "PATH": "%s:%s" % (b, os.environ["PATH"])}
    return root, log, env


def _bash(body, *args, env=None):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s' % (PROVISION, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _remove(env, user):
    return _bash('strih_dante_cpu_leftovers_remove "$1"', user, env=env)


def _calls(log):
    return [c for c in log.read_text().splitlines() if c]


# --- the removal ------------------------------------------------------------------------------------------

def test_every_leftover_is_removed_and_both_managers_re_exec_and_the_hub_reloads(tmp_path):
    root, log, env = _box(tmp_path)
    r = _remove(env, _user())
    assert r.returncode == 0, r.stdout + r.stderr
    for k, p in PATHS.items():
        assert not (root / p).exists(), k
    calls = _calls(log)
    assert "daemon-reexec" in calls and "--user -M %s@ daemon-reexec" % _user() in calls, calls
    assert "daemon-reload" in calls, calls
    for p in PATHS.values():
        assert ("/" + p) in r.stdout, r.stdout   # each removal is named


def test_a_clean_box_is_quiet_and_never_re_executes_or_reloads(tmp_path):
    _, log, env = _box(tmp_path, which=())
    r = _remove(env, _user())
    assert r.returncode == 0 and r.stdout == "" and r.stderr == "", r.stdout + r.stderr
    assert _calls(log) == []


def test_the_hub_drop_in_alone_reloads_and_never_re_executes(tmp_path):
    _, log, env = _box(tmp_path, which=("hub",))
    assert _remove(env, _user()).returncode == 0
    assert _calls(log) == ["daemon-reload"]


def test_a_manager_drop_in_alone_re_executes_and_never_reloads(tmp_path):
    _, log, env = _box(tmp_path, which=("user",))
    assert _remove(env, _user()).returncode == 0
    calls = _calls(log)
    assert "daemon-reexec" in calls and "daemon-reload" not in calls, calls


def test_no_user_re_executes_pid_1_only(tmp_path):
    _, log, env = _box(tmp_path, which=("system",))
    assert _remove(env, "").returncode == 0
    assert _calls(log) == ["daemon-reexec"]


def test_the_conf_helper_and_sudoers_alone_touch_no_manager(tmp_path):
    root, log, env = _box(tmp_path, which=("conf", "helper", "sudoers"))
    assert _remove(env, _user()).returncode == 0
    assert _calls(log) == [] and not (root / PATHS["helper"]).exists()


def test_a_foreign_file_of_a_header_checked_name_is_left_and_named(tmp_path):
    root, log, env = _box(tmp_path, which=())
    for k in ("system", "hub"):
        f = root / PATHS[k]
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text("# the operator's own\n[Manager]\n")
    r = _remove(env, _user())
    assert r.returncode == 0, r.stderr
    assert (root / PATHS["system"]).exists() and (root / PATHS["hub"]).exists()
    assert r.stderr.count("not ours") == 2, r.stderr
    assert _calls(log) == []


def test_a_failed_re_exec_is_a_warning_and_a_failed_reload_fails_by_name(tmp_path):
    root, log, env = _box(tmp_path, which=("system", "hub"))
    (tmp_path / "bin" / "systemctl").write_text('#!/bin/bash\nprintf "%s\\n" "$*" >> "{}"\nexit 1\n'.format(log))
    r = _remove(env, "")
    assert r.returncode == 1, r.stdout + r.stderr
    assert "WARNING" in r.stderr and "PID 1" in r.stderr, r.stderr
    assert "daemon-reload" in r.stderr, r.stderr


def test_each_call_is_bounded_by_timeout_30(tmp_path):
    _, log, env = _box(tmp_path, which=("system", "hub"))
    b = tmp_path / "bin"
    t = b / "timeout"
    t.write_text('#!/bin/bash\nprintf "timeout %%s\\n" "$*" >> "%s"\nshift\nexec "$@"\n' % log)
    t.chmod(0o755)
    assert _remove(env, _user()).returncode == 0
    calls = _calls(log)
    assert "timeout 30 systemctl daemon-reexec" in calls, calls
    assert "timeout 30 systemctl --user -M %s@ daemon-reexec" % _user() in calls, calls
    assert "timeout 30 systemctl daemon-reload" in calls, calls


# --- step 13 runs it on every box --------------------------------------------------------------------------

def _install(tmp_path, box, root_env):
    site = tmp_path / "site.conf"
    site.write_text("RX1=Program L|foh-tx|MASTER-L\nRX2=Program R|foh-tx|MASTER-R\n")
    pin = tmp_path / "pin"
    pin.write_text("OBS_RELEASE_DSTACK_VERSION=v0.6.19\nINTERCOM_RELEASE_DSTACK_VERSION=v0.6.19\n")
    env = dict(root_env, STRIH_DANTE_INPUT_TOML=str(tmp_path / "home" / ".camera-box" / "dante-input.toml"),
               STRIH_DANTE_INPUT_IFACE="enp7s0.70", STRIH_DANTE_INPUT_SITE_FILE=str(site),
               STRIH_DANTE_PTP_OBS_PIN=str(pin), STRIH_DANTE_PTP_INTERCOM_PIN=str(pin))
    return _bash('. scripts/lib/obs-box-baseline.sh; strih_box_load "$1" >/dev/null; strih_dante_input_install "$2" "$3"',
                 box, _user(), tmp_path / "home", env=env)


def test_step_13_removes_the_leftovers_on_the_dante_box_and_writes_no_reservation(tmp_path):
    root, log, env = _box(tmp_path)
    r = _install(tmp_path, "strih-snv", env)
    assert r.returncode == 0, r.stdout + r.stderr
    for k, p in PATHS.items():
        assert not (root / p).exists(), k
    assert "daemon-reexec" in _calls(log)


def test_step_13_removes_the_leftovers_on_a_box_without_the_dante_program(tmp_path):
    root, _, env = _box(tmp_path)
    r = _install(tmp_path, "strih-pp", env)
    assert r.returncode == 0, r.stdout + r.stderr
    for k, p in PATHS.items():
        assert not (root / p).exists(), k


# --- verify item 9 ---------------------------------------------------------------------------------------

GRADE = 'ok() { echo "OK $*"; }; bad() { echo "BAD $*"; }; note() { echo "NOTE $*"; }; strih_dante_cpu_leftovers_grade'


def test_verify_fails_every_leftover_by_name(tmp_path):
    root, _, env = _box(tmp_path)
    r = _bash(GRADE, env=env)
    assert r.returncode == 0, r.stderr
    bad = [l for l in r.stdout.splitlines() if l.startswith("BAD (dante-cpu) leftover")]
    assert len(bad) == len(PATHS), r.stdout
    for p in PATHS.values():
        assert any(("/" + p) in l for l in bad), (p, bad)
    assert "setup-strih.sh step 13" in bad[0]


def test_verify_passes_a_clean_box(tmp_path):
    _, _, env = _box(tmp_path, which=())
    r = _bash(GRADE, env=env)
    lines = r.stdout.splitlines()
    assert len(lines) == 1 and lines[0].startswith("OK (dante-cpu)"), r.stdout


def test_verify_fails_a_foreign_file_at_a_leftover_name_too(tmp_path):
    root, _, env = _box(tmp_path, which=())
    f = root / PATHS["user"]
    f.parent.mkdir(parents=True)
    f.write_text("[Manager]\nCPUAffinity=0-3\n")
    r = _bash(GRADE, env=env)
    assert "BAD (dante-cpu)" in r.stdout and PATHS["user"] in r.stdout, r.stdout


def test_item9_grades_the_leftovers_on_both_program_audio_paths():
    lib = (SCRIPTS / "lib" / "strih-dante-input.sh").read_text()
    body = lib.split("strih_program_audio_grade() {", 1)[1].split("\n}", 1)[0]
    assert "strih_dante_cpu_leftovers_grade" in body, body
    assert body.index("strih_dante_cpu_leftovers_grade") < body.index("strih_program_dante_declared"), body


# --- the machinery is gone ---------------------------------------------------------------------------------

def test_the_pinner_libs_are_gone_and_nothing_sources_them():
    assert not (SCRIPTS / "lib" / "strih-dante-rx-pin.sh").exists()
    assert not (SCRIPTS / "lib" / "strih-dante-reserved-cpu.sh").exists()
    for f in list(SCRIPTS.glob("*.sh")) + list(SCRIPTS.glob("lib/*.sh")):
        t = f.read_text()
        assert "strih-dante-rx-pin.sh" not in t and "strih-dante-reserved-cpu.sh" not in t, f
        for fn in ("strih_dante_rx_pin_", "strih_dante_reserved_cpu", "_strih_dante_cpulist_has", "_strih_dante_hub_flag"):
            assert fn not in t, (f, fn)


def test_the_launcher_runs_obs_on_the_whole_mask_and_no_helper():
    s = (SCRIPTS / "strih-obs-start.sh").read_text()
    for gone in ("RX_PIN", "strih-dante-rx-cpu.conf", "strih-dante-rx-pin", "sudo -n"):
        assert gone not in s, gone


def test_the_dante_input_toml_follows_dlinks_default_latency(tmp_path):
    s = tmp_path / "site.conf"
    s.write_text("RX1=Program L|foh-tx|MASTER-L\n")
    r = _bash('strih_box_load strih-snv >/dev/null; strih_dante_input_toml enp7s0.70 "4440 8800 4455 47200" '
              '"$(strih_dante_input_site_rows "$1")"', s)
    assert r.returncode == 0, r.stderr
    assert "latency_ms" not in r.stdout, r.stdout
    assert "STRIH_DANTE_INPUT_LATENCY_MS" not in (SCRIPTS / "lib" / "strih-dante-input.sh").read_text()


# --- the JPEG XS cores back to the plain rule ------------------------------------------------------------

JXS = SCRIPTS / "lib" / "strih-jxs-provision.sh"


def _render(f, *args):
    return _bash('. "%s"; strih_jxs_output_render "$@"' % JXS, f, *args)


LIVE = ('{"enabled": true, "jxs_target": "10.77.9.204:7100", "jxs_stream_id": 100, "jxs_bpp_min": 0, '
        '"jxs_bpp_max": 0, "jxs_cores": "", "jxs_pace_mbps": 0}\n')


def test_a_derived_cores_value_is_reset_to_every_cpu_and_its_record_dropped(tmp_path):
    f = tmp_path / "jxs-output.json"
    d = json.loads(LIVE)
    d.update(jxs_cores="2-13", jxs_cores_derived="2-13")
    f.write_text(json.dumps(d))
    r = _render(f, "10.77.9.204:7100", 100)
    assert r.returncode == 0, r.stderr
    assert r.stdout == LIVE, r.stdout


def test_an_operators_cores_value_is_kept_and_a_stale_record_dropped(tmp_path):
    f = tmp_path / "jxs-output.json"
    d = json.loads(LIVE)
    d.update(jxs_cores="4-7", jxs_cores_derived="2-14")
    f.write_text(json.dumps(d))
    out = json.loads(_render(f, "10.77.9.204:7100", 100).stdout)
    assert out["jxs_cores"] == "4-7" and "jxs_cores_derived" not in out, out


def test_the_live_file_renders_byte_identical(tmp_path):
    f = tmp_path / "jxs-output.json"
    f.write_text(LIVE)
    assert _render(f, "10.77.9.204:7100", 100).stdout == LIVE


def test_verify_fails_a_derived_cores_record_left_in_the_file(tmp_path):
    f = tmp_path / "jxs-output.json"
    d = json.loads(LIVE)
    d.update(jxs_cores="2-13", jxs_cores_derived="2-13")
    f.write_text(json.dumps(d))
    r = _bash('. "%s"; ok() { echo "OK $*"; }; bad() { echo "BAD $*"; }; _strih_jxs_cores_row "$1"' % JXS, f)
    assert r.stdout.startswith("BAD (jxs-cores)") and "jxs_cores_derived" in r.stdout, r.stdout + r.stderr
    f.write_text(LIVE)
    r = _bash('. "%s"; ok() { echo "OK $*"; }; bad() { echo "BAD $*"; }; _strih_jxs_cores_row "$1"' % JXS, f)
    assert r.stdout.startswith("OK (jxs-cores)"), r.stdout + r.stderr


def test_the_jxs_lib_derives_nothing_from_the_dante_libs():
    t = JXS.read_text()
    for gone in ("strih_jxs_output_default_cores", "strih_jxs_output_cores_reconcile", "_strih_jxs_dante_lib",
                 "OPERATOR-CORES", "reserved Dante"):
        assert gone not in t, gone
