"""Issue 1387 (owner 2.11.2026): OBS on a broadcast box is immune to background load; backups are not
disabled or time-boxed.

scripts/lib/obs-box-background.sh is sourced under `set -euo pipefail` for the pure logic. The installer,
the real gather and the bench run against stubs and moved paths, so nothing real is touched.
"""
import json
import os
import stat
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "obs-box-background.sh"
BASE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
VERIFY = REPO / "scripts" / "lib" / "obs-box-baseline-verify.sh"
BENCH = REPO / "scripts" / "background-load-bench.sh"
DECISION = REPO / "scripts" / "background_load_decision.py"
DECODE = REPO / "scripts" / "recording-verdict-on-strih-snv.sh"
PIXELS = REPO / "scripts" / "lib" / "missing-slot-pixels.sh"


def bash(script, env=None, stdin=None):
    full = {**os.environ, **(env or {})}
    return subprocess.run(
        ["bash", "-c", "set -euo pipefail\n" + script], capture_output=True, text=True, env=full, input=stdin
    )


def lib(call, env=None, stdin=None):
    return bash(f". {LIB}\n{call}", env, stdin)


def test_cpulist_expand_and_compress_round_trip():
    assert lib("obs_box_cpulist_expand 0-1,12-15").stdout.strip() == "0 1 12 13 14 15"
    assert lib("obs_box_cpulist_expand 3,1,2,2").stdout.strip() == "1 2 3"
    assert lib("obs_box_cpulist_compress '0 1 3 5 6 7'").stdout.strip() == "0-1,3,5-7"


def test_cpulist_expand_refuses_a_malformed_list():
    for bad in ("", "a", "1-", "-1", "3-1", "1--2", "1,,2", "1-2-3"):
        assert lib(f"obs_box_cpulist_expand '{bad}'").returncode == 1, bad


def test_background_cpus_is_the_complement_of_the_obs_mask():
    # strih-snv: 16 threads, OBS 2-15 as the plan persists it
    obs = ",".join(str(c) for c in range(2, 16))
    assert lib(f"obs_box_background_cpus 0-15 {obs}").stdout.strip() == "0-1"
    # imag: the housekeeping pair + the four E-cores
    assert lib("obs_box_background_cpus 0-15 2-11").stdout.strip() == "0-1,12-15"


def test_background_cpus_refuses_when_nothing_is_left_or_a_list_is_unread():
    assert lib("obs_box_background_cpus 0-3 0-3").returncode == 1
    assert lib("obs_box_background_cpus '' 0-3").returncode == 1
    assert lib("obs_box_background_cpus 0-3 ''").returncode == 1


def test_smt_shared_names_a_background_cpu_whose_sibling_is_obs():
    pairs = "0 0-1\n1 0-1\n2 2-3\n3 2-3\n"
    assert lib("obs_box_background_smt_shared 0-1 2-3", stdin=pairs).stdout.strip() == ""
    # OBS on cpu 1 beside a background cpu 0, and the comma form of a sibling list
    assert lib("obs_box_background_smt_shared 0 1-3", stdin=pairs).stdout.strip() == "0"
    assert lib("obs_box_background_smt_shared 0 8", stdin="0 0,8\n8 0,8\n").stdout.strip() == "0"
    assert lib("obs_box_background_smt_shared 0 8", stdin="0 junk\n").stdout.strip() == "unread"
    assert lib("obs_box_background_smt_shared 0 8", stdin="").stdout.strip() == "unread"


def test_slice_text_derives_cpus_and_keeps_memory_soft():
    text = lib("obs_box_background_slice_text 0-1").stdout
    assert "AllowedCPUs=0-1\n" in text
    assert "CPUWeight=1\n" in text and "IOWeight=1\n" in text and "MemoryHigh=30%\n" in text
    assert "MemoryMax" not in text, "a soft bound only: the backup is never OOM-killed"
    assert lib("obs_box_background_slice_text ''").returncode != 0


def test_dropin_only_moves_the_unit_into_the_slice():
    text = lib("obs_box_background_dropin_text").stdout
    lines = [ln for ln in text.splitlines() if ln and not ln.startswith("#")]
    assert lines == ["[Service]", "Slice=rig-background.slice"]


def _helper(tmp, env_extra):
    """The real runner with a logging systemd-run on PATH."""
    b = tmp / "bin"
    b.mkdir()
    run = b / "systemd-run"
    run.write_text('#!/bin/bash\nprintf "%s\\n" "$@" > "$RUN_LOG"\n')
    run.chmod(0o755)
    idf = b / "id"
    idf.write_text("#!/bin/bash\necho 0\n")
    idf.chmod(0o755)
    gp = b / "getent"
    gp.write_text("#!/bin/bash\necho 'u:x:1000:1000::/home/u:/bin/bash'\n")
    gp.chmod(0o755)
    helper = tmp / "obs-box-background-run"
    text = lib("obs_box_background_run_text").stdout
    assert "\nPATH=/usr/sbin:/usr/bin:/sbin:/bin\n" in text
    helper.write_text(text.replace("\nPATH=/usr/sbin:/usr/bin:/sbin:/bin\n", f"\nPATH={b}:/usr/sbin:/usr/bin:/sbin:/bin\n"))
    helper.chmod(0o755)
    log = tmp / "run.log"
    env = {**os.environ, "PATH": f"{b}:{os.environ['PATH']}", "RUN_LOG": str(log), **env_extra}
    return helper, log, env


def test_runner_starts_a_scope_in_the_slice_as_the_caller(tmp_path):
    helper, log, env = _helper(tmp_path, {"SUDO_UID": "1000", "SUDO_GID": "1000"})
    r = subprocess.run([str(helper), "env", "RUST_LOG=info", "/tmp/bin", "--a b"], env=env, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    argv = log.read_text().splitlines()
    assert argv[:4] == ["--quiet", "--scope", "--collect", "--slice=rig-background.slice"]
    assert not any(a.startswith(("--uid", "--gid")) for a in argv[:5]), "they keep root's group 0"
    assert argv[4:] == [
        "--", "setpriv", "--reuid=1000", "--regid=1000", "--init-groups",
        "nice", "-n", "19", "ionice", "-c", "3", "env", "HOME=/home/u", "env", "RUST_LOG=info", "/tmp/bin", "--a b",
    ]


def test_runner_refuses_root_and_a_missing_caller(tmp_path):
    helper, log, env = _helper(tmp_path, {})
    for extra in ({"SUDO_UID": "0", "SUDO_GID": "0"}, {"SUDO_UID": "", "SUDO_GID": ""}, {"SUDO_UID": "1000", "SUDO_GID": ""}):
        r = subprocess.run([str(helper), "true"], env={**env, **extra}, capture_output=True, text=True)
        assert r.returncode == 2, extra
    assert not log.exists()
    r = subprocess.run([str(helper)], env={**env, "SUDO_UID": "1000", "SUDO_GID": "1000"}, capture_output=True, text=True)
    assert r.returncode == 2


def _install(tmp, active="", group="/system.slice/fbagent@main.service", conf="2,3,4,5,6,7", visudo_rc=0):
    """Run the real installer with its host tools stubbed; returns (result, systemctl calls)."""
    root = tmp / "root"
    (root / "etc" / "sudoers.d").mkdir(parents=True, exist_ok=True)
    if conf is not None:
        (root / "etc" / "strih-isolated-cpus.conf").write_text(conf + "\n")
    online = tmp / "online"
    online.write_text("0-7\n")
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    calls = tmp / "systemctl.calls"
    (b / "systemctl").write_text(
        "#!/bin/bash\n"
        f'echo "$*" >> {calls}\n'
        'case "$1" in\n'
        f"  list-units) printf '%b' '{active}' ;;\n"
        f"  show) echo {group} ;;\n"
        "esac\n"
    )
    (b / "visudo").write_text(f"#!/bin/bash\nexit {visudo_rc}\n")
    for f in b.iterdir():
        f.chmod(0o755)
    owner = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    group = subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip()
    r = bash(
        f'fail() {{ echo "FAIL: $*" >&2; exit 9; }}\nYELLOW=; NC=\n. {BASE}\nobs_box_background_slice strih newlevel',
        {
            "PATH": f"{b}:{os.environ['PATH']}",
            "OBS_BOX_BACKGROUND_ROOT": str(root),
            "OBS_BOX_BACKGROUND_OWNER": f"{owner}:{group}",
            "OBS_BOX_ONLINE_CPUS": str(online),
        },
    )
    text = calls.read_text() if calls.exists() else ""
    return r, text, root


def test_install_writes_every_file_and_never_touches_a_service(tmp_path):
    r, calls, root = _install(tmp_path)
    assert r.returncode == 0, r.stderr
    slice_text = (root / "etc/systemd/system/rig-background.slice").read_text()
    assert "AllowedCPUs=0-1\n" in slice_text
    assert (root / "etc/systemd/system/fbagent@.service.d/50-rig-background.conf").is_file()
    runner = root / "usr/local/sbin/obs-box-background-run"
    assert runner.stat().st_mode & stat.S_IXUSR
    sudoers = root / "etc/sudoers.d/obs-box-background"
    assert stat.S_IMODE(sudoers.stat().st_mode) == 0o440
    assert "newlevel ALL=(root) NOPASSWD: /usr/local/sbin/obs-box-background-run\n" in sudoers.read_text()
    assert "daemon-reload" in calls
    assert "enable --now rig-background.slice" in calls
    for verb in ("restart", "stop", "start ", "kill", "try-restart", "reload-or-restart"):
        assert not any(ln.startswith(verb) for ln in calls.splitlines()), calls


def test_install_rerun_is_quiet(tmp_path):
    _install(tmp_path)
    (tmp_path / "systemctl.calls").unlink()
    r, calls, _ = _install(tmp_path)
    assert r.returncode == 0, r.stderr
    assert "daemon-reload" not in calls
    assert "unchanged" in r.stdout


def test_install_fences_a_running_instance_live_and_only_outside_the_slice(tmp_path):
    active = "fbagent@main.service loaded active running x\\n"
    r, calls, _ = _install(tmp_path, active=active)
    assert r.returncode == 0, r.stderr
    assert "set-property --runtime fbagent@main.service AllowedCPUs=0-1 CPUWeight=1 IOWeight=1 MemoryHigh=30%" in calls
    assert "--state=active,activating" in calls
    r, calls, _ = _install(tmp_path, active=active, group="/rig-background.slice/fbagent@main.service")
    assert calls.count("set-property") == 1, "already in the slice: nothing more to do"


def test_install_refuses_without_an_obs_mask_and_on_a_bad_sudoers_line(tmp_path):
    a = tmp_path / "a"
    a.mkdir()
    r, _, root = _install(a, conf=None)
    assert r.returncode == 9 and "no CPU left" in r.stderr
    assert not (root / "etc/systemd/system/rig-background.slice").exists()
    b = tmp_path / "b"
    b.mkdir()
    r, _, root = _install(b, visudo_rc=1)
    assert r.returncode == 9
    assert not (root / "etc/sudoers.d/obs-box-background").exists()


GOOD = {
    "online": "0-15",
    "obs": "2,3,4,5,6,7,8,9,10,11,12,13,14,15",
    "loaded": "loaded",
    "allowed": "0-1",
    "file": "1",
    "run": "1",
    "sudo": "1",
    "fbagent": "1",
    "dropin": "1",
    "pgrep": "1",
    "procs": "restic:9:0-1 fbagent:8:0-1",
    "obs_running": "1",
    "obs_mask": "2-15",
    "smt": "",
    "cgroup": "0-1",
    "iosched": "[none] mq-deadline",
    "enabled": "enabled",
}


def grade(**over):
    f = {**GOOD, **over}
    args = " ".join("'%s'" % f[k] for k in GOOD)
    return lib(f"obs_box_background_grade {args}")


def test_grade_passes_a_fenced_box():
    r = grade()
    assert r.returncode == 0, r.stdout
    assert "CPUs=0-1" in r.stdout and "[none] mq-deadline" in r.stdout


def test_grade_fails_each_broken_fact():
    table = {
        "slice-not-loaded": {"loaded": "not-found"},
        "slice-AllowedCPUs": {"allowed": "0-3"},
        "slice-file-differs": {"file": "0"},
        "runner-missing": {"run": "0"},
        "runner-not-allowed": {"sudo": "0"},
        "fbagent-drop-in-missing": {"dropin": "0"},
        "fbagent-unread": {"fbagent": ""},
        "pgrep-missing": {"pgrep": "0"},
        "outside-the-slice": {"procs": "restic:9:0-15"},
        "restic-mask-unread": {"procs": "restic:9:unread"},
        "OBS-runs-on-background-cpu": {"obs_mask": "0-15"},
        "obs-mask-unread": {"obs_mask": ""},
        "SMT-shared": {"smt": "1"},
        "cgroup-cpus": {"cgroup": "0-15"},
        "cgroup-cpus(unread)": {"cgroup": ""},
        "slice-not-enabled": {"enabled": "disabled"},
        "SMT-shared-with-OBS(unread)": {"smt": "unread"},
    }
    for name, over in table.items():
        r = grade(**over)
        assert r.returncode == 1 and name in r.stdout, (name, r.stdout)


def test_grade_without_fbagent_needs_no_drop_in_and_an_idle_box_has_no_procs():
    r = grade(fbagent="0", dropin="0", procs="", obs_running="0", obs_mask="")
    assert r.returncode == 0, r.stdout


def test_grade_fails_closed_on_an_unreadable_fence():
    r = grade(online="")
    assert r.returncode == 1 and "FENCE UNREADABLE" in r.stdout


def _gather(tmp, procs=""):
    """The real gather, its /etc, /sys and /proc paths moved into TMP; the tools it calls stubbed."""
    text = bash(f". {VERIFY}\nobs_box_baseline_gather_snippet strih newlevel strih-obs.service").stdout
    sysroot = tmp / "sys"
    cpu = sysroot / "devices/system/cpu"
    for c in range(4):
        d = cpu / f"cpu{c}/topology"
        d.mkdir(parents=True)
        (d / "thread_siblings_list").write_text(("0-1" if c < 2 else "2-3") + "\n")
    (cpu / "online").write_text("0-3\n")
    etc = tmp / "etc"
    etc.mkdir()
    (etc / "strih-isolated-cpus.conf").write_text("2,3\n")
    proc = tmp / "proc"
    proc.mkdir()
    b = tmp / "bin"
    b.mkdir()
    (b / "pgrep").write_text(
        '#!/bin/bash\ncase "$*" in\n  "-x restic") echo 77 ;;\n  "-xo obs") echo 55 ;;\nesac\n' if procs else "#!/bin/bash\nexit 1\n"
    )
    (b / "systemctl").write_text(
        '#!/bin/bash\ncase "$*" in\n  *LoadState*) echo loaded ;;\n  *AllowedCPUs*) echo 0-1 12-13 ;;\n'
        '  list-unit-files*) echo "fbagent@.service enabled" ;;\nesac\n'
    )
    (b / "sudo").write_text("#!/bin/bash\nexit 0\n")
    for f in b.iterdir():
        f.chmod(0o755)
    if procs:
        for pid, mask in ((77, procs), (55, "2-3")):
            (proc / str(pid)).mkdir()
            (proc / str(pid) / "status").write_text(f"Name:\tx\nCpus_allowed_list:\t{mask}\n")
    for old, new in (
        ("/sys/devices/system/cpu", str(cpu)),
        ('"/etc/${BOX}-isolated-cpus.conf"', f'"{etc}/${{BOX}}-isolated-cpus.conf"'),
        ('"/proc/${_bg_p}', f'"{proc}/${{_bg_p}}'),
        ('"/proc/${_op}/status"', f'"{proc}/${{_op}}/status"'),
    ):
        assert old in text, old
        text = text.replace(old, new)
    r = subprocess.run(
        ["bash", "-c", text], capture_output=True, text=True, env={**os.environ, "PATH": f"{b}:{os.environ['PATH']}"}
    )
    return dict(ln.split("=", 1) for ln in r.stdout.splitlines() if "=" in ln)


def test_real_gather_reads_a_running_restic_and_the_live_obs_mask(tmp_path):
    f = _gather(tmp_path, procs="0-3")
    assert f["bg_online"] == "0-3"
    assert f["bg_loaded"] == "loaded" and f["bg_allowed"] == "0-1,12-13", "space separated from systemctl show"
    assert f["bg_fbagent"] == "1" and f["bg_pgrep"] == "1"
    assert f["bg_procs"] == "restic:77:0-3"
    assert f["bg_obs_mask"] == "2-3"
    assert f["bg_smt_shared"] == ""
    r = lib(
        f"obs_box_background_grade '{f['bg_online']}' 2,3 loaded 0-1 1 1 1 1 1 1 '{f['bg_procs']}' 1 2-3 '' 0-1 '' enabled"
    )
    assert r.returncode == 1 and "outside-the-slice" in r.stdout


def test_decode_launch_uses_the_runner_when_the_box_has_it(tmp_path):
    src = DECODE.read_text()
    start = src.index("strih_lx_lowprio_prefix() {")
    end = src.index("\n}\n", start) + 3
    fn = src[start:end]
    runner = tmp_path / "run"
    runner.write_text("#!/bin/bash\n")
    runner.chmod(0o755)
    (tmp_path / "devices/cpu_atom").mkdir(parents=True)
    (tmp_path / "devices/cpu_atom/cpus").write_text("12-15\n")
    fake = tmp_path / "bin"
    fake.mkdir()
    sudo = fake / "sudo"
    sudo.write_text("#!/bin/bash\nexit 0\n")
    sudo.chmod(0o755)
    env = {"PATH": f"{fake}:{os.environ['PATH']}"}
    assert bash(fn + f"strih_lx_lowprio_prefix {tmp_path} {runner}", env).stdout == f"sudo -n {runner}"
    assert bash(fn + f"strih_lx_lowprio_prefix {tmp_path} /nonexistent", env).stdout == "nice -n 19 taskset -c 12-15"
    sudo.write_text("#!/bin/bash\nexit 1\n")
    assert bash(fn + f"strih_lx_lowprio_prefix {tmp_path} {runner}", env).stdout == "nice -n 19 taskset -c 12-15"


def test_the_remote_snippet_picks_the_runner_last_and_both_copies_agree():
    snippet = '''R=/usr/local/sbin/obs-box-background-run; if [ -x "$R" ] && sudo -n -l "$R" >/dev/null 2>&1; then LP="sudo -n $R"; fi;'''
    assert snippet in DECODE.read_text()
    assert snippet in PIXELS.read_text()
    assert lib("obs_box_background_run_path").stdout.strip() == "/usr/local/sbin/obs-box-background-run"


def audit(*lagged):
    return "".join(
        f"12:00:00.000: program-render-audit: render_fps=30.0 target_fps=30.0 avg_frame_ms=23.00 lagged={n} total=150\n"
        for n in lagged
    )


def decide(tmp, baseline, inside, outside, *extra):
    for name, text in (("b", baseline), ("i", inside), ("o", outside)):
        (tmp / name).write_text(text)
    loads = ["--inside-load", "124:16:16", "--outside-load", "124:16:0"]
    r = subprocess.run(
        ["python3", str(DECISION), "--baseline", str(tmp / "b"), "--inside", str(tmp / "i"), "--outside", str(tmp / "o"), *loads, *extra],
        capture_output=True,
        text=True,
    )
    return r.returncode, json.loads(r.stdout)


def test_decision_pass_needs_a_load_that_bites_outside(tmp_path):
    rc, out = decide(tmp_path, audit(*[0] * 6), audit(*[0] * 6), audit(0, 0, 19, 0, 40, 0))
    assert rc == 0 and out["verdict"] == "PASS" and out["outside"]["lagged"] == 59


def test_decision_grades_every_other_shape(tmp_path):
    quiet = audit(*[0] * 6)
    bites = audit(*[5] * 6)
    assert decide(tmp_path, quiet, audit(0, 0, 0, 0, 0, 1), bites)[0] == 1
    assert decide(tmp_path, quiet, quiet, quiet)[0] == 2, "a load that never bites proves nothing"
    assert decide(tmp_path, audit(0, 0, 0, 0, 0, 3), quiet, bites)[0] == 2, "a noisy baseline proves nothing"
    assert decide(tmp_path, quiet, audit(0), bites)[0] == 2, "too few audit lines"
    assert decide(tmp_path, "", "", "")[0] == 2
    rc, out = decide(tmp_path, quiet, quiet, bites, "--mask-before", "2-15", "--mask-after", "0-15")
    assert rc == 1 and "mask moved" in out["reason"]


def test_decision_never_passes_a_load_it_cannot_prove(tmp_path):
    quiet, bites = audit(*[0] * 6), audit(*[5] * 6)
    for flag, proof in (
        ("--inside-load", "1:0:0"),  # sudo or the slice failed: no load ran, lagged 0 is worth nothing
        ("--inside-load", "124:0:0"),
        ("--inside-load", "124:16:15"),  # one process escaped the slice
        ("--inside-load", ""),
        ("--outside-load", "124:16:16"),  # the harmful phase ran in the slice
        ("--outside-load", "255:16:0"),
    ):
        rc, out = decide(tmp_path, quiet, quiet, bites, flag, proof)
        assert rc == 2 and out["verdict"] == "UNKNOWN", (flag, proof, out)


def test_bench_plan_touches_nothing_and_reads_the_log_bounded():
    r = subprocess.run(["bash", str(BENCH)], capture_output=True, text=True, env={**os.environ, "PATH": os.environ["PATH"]})
    assert r.returncode == 0, r.stderr
    assert "nothing below has run" in r.stdout
    assert "sudo -n /usr/local/sbin/obs-box-background-run timeout 180" in r.stdout
    assert "nice -n 19 ionice -c 3 timeout 180" in r.stdout
    assert "tail -c +1" in r.stdout and "head -c 5000000" in r.stdout
    text = BENCH.read_text()
    assert "wc -l" not in text, "never counts the lines of a 100 MB log"
    for forbidden in ("SetCurrentProgramScene", "StartRecord", "speaker-test", "aplay"):
        assert forbidden not in text


def test_bench_refuses_usage_errors_before_any_network():
    env = {k: v for k, v in os.environ.items() if k not in ("STRIH_USER", "STRIH_PW", "CAM_PW")}
    r = subprocess.run(["bash", str(BENCH), "--run"], capture_output=True, text=True, env=env)
    assert r.returncode == 3 and "STRIH_USER" in r.stderr
    r = subprocess.run(["bash", str(BENCH), "--phase-s", "30"], capture_output=True, text=True, env=env)
    assert r.returncode == 3
    r = subprocess.run(["bash", str(BENCH), "--box", "nosuch"], capture_output=True, text=True, env=env)
    assert r.returncode == 3


def test_the_lib_stays_within_the_setup_budgets():
    assert len((REPO / "scripts" / "setup-strih.sh").read_text().splitlines()) <= 1000
    for setup in ("setup-strih.sh", "setup-imag.sh"):
        assert "obs_box_background_slice " in (REPO / "scripts" / setup).read_text()
