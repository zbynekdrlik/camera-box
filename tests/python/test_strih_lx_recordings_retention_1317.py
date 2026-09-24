"""issue 1317 part 5 -- the Linux (strih-lx) recordings-retention executor.

strih-lx (the Linux strih, fleet row ``strih-lx|10.77.9.202|linux-genlock``) records one E2E run per
capture into ``/srv/_REC`` (its active OBS profile ``[AdvOut] RecFilePath``), and until this part NO
retention ran there: ``scripts/strih-recordings-retention.sh`` refused every linux-genlock address
(part 3) and its only executor was the Windows ``.ps1``.

This part adds the Linux executor to the SAME wrapper:

  * ``--local-sweep`` -- a bash port of the canonical pure decision ``src/recordings_retention.rs``
    ``plan()`` (the OBS-timestamp allowlist, the 1 GiB production-size floor, then newest-N UNION
    younger-than-D), run on the current machine over a record dir (``--record-dir``, or resolved from
    the box's active OBS profile). DRY-RUN by default; ``--execute`` deletes ONLY the DELETE set.
  * ``--box <fleet-name>`` -- the fleet-list CLASS dispatch (the obs-backup-retention.sh precedent):
    linux-genlock -> ssh + ``bash -s -- --local-sweep`` over the box; windows-genlock -> the unchanged
    ``.ps1`` driver.

PARITY: ``tests/fixtures/recordings_retention_parity.tsv`` is ONE table read by BOTH
``tests/recordings_retention.rs`` (against the Rust ``plan()``) and the parity test below (against the
bash decision over a REAL fixture directory -- sparse files with the table's sizes and mtimes). Both
assert the same expected sets, so the bash executor is pinned to the Rust authority on identical input.

Tier-0: bash + python only, fake ``sshpass`` on PATH, no network, no rig.
"""

import os
import re
import stat
import subprocess
import tempfile

_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
REC = os.path.join(_ROOT, "scripts", "strih-recordings-retention.sh")
RS = os.path.join(_ROOT, "src", "recordings_retention.rs")
TABLE = os.path.join(_ROOT, "tests", "fixtures", "recordings_retention_parity.tsv")


# --- the shared parity table --------------------------------------------------------------------

def _parse_table():
    cases, cur = [], None
    with open(TABLE, encoding="utf-8") as fh:
        for lineno, raw in enumerate(fh, 1):
            line = raw.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            f = line.split("\t")
            if f[0] == "case":
                assert cur is None and len(f) == 5, (lineno, line)
                cur = {"id": f[1], "now": int(f[2]), "runs": f[3], "days": f[4],
                       "files": [], "keep": [], "delete": []}
            elif f[0] == "file":
                assert len(f) == 4, (lineno, line)
                cur["files"].append((int(f[1]), int(f[2]), f[3]))
            elif f[0] == "keep":
                assert len(f) == 3, (lineno, line)
                cur["keep"].append((f[1], f[2]))
            elif f[0] == "delete":
                assert len(f) == 2, (lineno, line)
                cur["delete"].append(f[1])
            elif f[0] == "end":
                cases.append(cur)
                cur = None
            else:
                raise AssertionError((lineno, line))
    assert cur is None
    return cases


def _populate(d, case):
    for age, size, name in case["files"]:
        p = os.path.join(d, name)
        with open(p, "wb") as fh:
            fh.truncate(size)  # sparse: a 17 GiB fixture costs no disk
        mt = case["now"] - age
        os.utime(p, (mt, mt))


def _env(extra=None):
    env = dict(os.environ)
    for k in ("OBS_FLEET", "STRIH_LX_HOST", "RETENTION_NOW_EPOCH"):
        env.pop(k, None)
    env.update(extra or {})
    return env


def _run(args, env=None):
    return subprocess.run(["bash", REC] + args, capture_output=True, text=True,
                          env=env if env is not None else _env(), stdin=subprocess.DEVNULL,
                          timeout=120)


def _plan_rows(stdout):
    """The machine plan (--plan-tsv): PROTECT/KEEP/DELETE rows, tab-separated, name last."""
    keep, delete = [], []
    for line in stdout.splitlines():
        f = line.split("\t")
        if f[0] == "PROTECT":
            keep.append(("protected", f[-1]))
        elif f[0] == "KEEP":
            keep.append((f[1], f[-1]))
        elif f[0] == "DELETE":
            delete.append(f[-1])
    return keep, delete


def test_parity_table_is_non_trivial():
    cases = _parse_table()
    assert len(cases) >= 10
    assert any(len(c["delete"]) >= 5 for c in cases)


def test_bash_decision_matches_the_shared_parity_table():
    for case in _parse_table():
        with tempfile.TemporaryDirectory() as d:
            _populate(d, case)
            r = _run(["--local-sweep", "--record-dir", d, "--keep-runs", case["runs"],
                      "--keep-days", case["days"], "--plan-tsv"],
                     env=_env({"RETENTION_NOW_EPOCH": str(case["now"])}))
            assert r.returncode == 0, (case["id"], r.stdout + r.stderr)
            keep, delete = _plan_rows(r.stdout)
            assert sorted(keep) == sorted(case["keep"]), (case["id"], r.stdout)
            assert delete == case["delete"], (case["id"], r.stdout)
            # dry-run: nothing left the directory.
            assert len(os.listdir(d)) == len(case["files"]), case["id"]


def test_size_floor_constant_is_byte_identical_to_the_rust_authority():
    rs = open(RS, encoding="utf-8").read()
    m = re.search(r"pub const PRODUCTION_SIZE_FLOOR_BYTES:\s*u64\s*=\s*([0-9_]+)\s*;", rs)
    assert m
    sh = open(REC, encoding="utf-8").read()
    m2 = re.search(r"^RR_PRODUCTION_SIZE_FLOOR_BYTES=([0-9]+)$", sh, re.MULTILINE)
    assert m2, "the bash mirror must carry its own floor constant"
    assert int(m.group(1).replace("_", "")) == int(m2.group(1)) == 1073741824


# --- the executor ------------------------------------------------------------------------------

def _mk(d, name, size, age, now):
    p = os.path.join(d, name)
    with open(p, "wb") as fh:
        fh.truncate(size)
    os.utime(p, (now - age, now - age))
    return p


def test_execute_deletes_only_the_delete_set():
    now = 1800000000
    day = 86400
    with tempfile.TemporaryDirectory() as d:
        _mk(d, "2026-01-01 10-00-00.mkv", 10, 30 * day, now)        # delete
        _mk(d, "2026-01-02 10-00-00.mkv", 10, 20 * day, now)        # delete
        _mk(d, "2026-01-03 10-00-00.mkv", 10, 1 * day, now)         # newest-run
        _mk(d, "2026-01-04 10-00-00.mkv", 2 * 1073741824, 90 * day, now)  # production-sized
        _mk(d, "strih700105.mkv", 10, 900 * day, now)               # protected
        os.mkdir(os.path.join(d, "2026-01-05 10-00-00.mkv"))        # a dir: never touched
        r = _run(["--local-sweep", "--record-dir", d, "--keep-runs", "1", "--keep-days", "0",
                  "--execute"], env=_env({"RETENTION_NOW_EPOCH": str(now)}))
        assert r.returncode == 0, r.stdout + r.stderr
        left = sorted(os.listdir(d))
        assert left == ["2026-01-03 10-00-00.mkv", "2026-01-04 10-00-00.mkv",
                        "2026-01-05 10-00-00.mkv", "strih700105.mkv"], left
        assert "EXECUTE" in r.stdout


def test_dry_run_is_the_default_and_deletes_nothing():
    now = 1800000000
    with tempfile.TemporaryDirectory() as d:
        _mk(d, "2026-01-01 10-00-00.mkv", 10, 400 * 86400, now)
        r = _run(["--local-sweep", "--record-dir", d, "--keep-runs", "0", "--keep-days", "0"],
                 env=_env({"RETENTION_NOW_EPOCH": str(now)}))
        assert r.returncode == 0, r.stdout + r.stderr
        assert "DRY-RUN" in r.stdout and "2026-01-01 10-00-00.mkv" in r.stdout
        assert os.listdir(d) == ["2026-01-01 10-00-00.mkv"]


def test_execute_with_zero_keep_runs_is_refused():
    # --keep-runs 0 could delete the recording OBS is writing right now; the executor refuses it.
    with tempfile.TemporaryDirectory() as d:
        _mk(d, "2026-01-01 10-00-00.mkv", 10, 400 * 86400, 1800000000)
        r = _run(["--local-sweep", "--record-dir", d, "--keep-runs", "0", "--keep-days", "0",
                  "--execute"])
        assert r.returncode == 2, r.stdout + r.stderr
        assert "--keep-runs" in r.stderr
        assert os.listdir(d) == ["2026-01-01 10-00-00.mkv"]


def test_missing_record_dir_fails_loud():
    r = _run(["--local-sweep", "--record-dir", "/nonexistent/_REC"])
    assert r.returncode == 1, r.stdout + r.stderr
    assert "not found" in r.stderr


def test_bad_policy_numbers_are_a_usage_error():
    with tempfile.TemporaryDirectory() as d:
        for args in (["--keep-runs", "x"], ["--keep-runs", "-1"], ["--keep-days", "1e-5"],
                     ["--keep-days", "abc"]):
            r = _run(["--local-sweep", "--record-dir", d] + args)
            assert r.returncode == 2, (args, r.stdout + r.stderr)


# --- record dir resolved from the box's active OBS profile ------------------------------------

def _obs_cfg(root, profile, basic_ini, user_ini=True):
    cfg = os.path.join(root, "obs-studio")
    os.makedirs(os.path.join(cfg, "basic", "profiles", profile))
    if user_ini:
        with open(os.path.join(cfg, "user.ini"), "w") as fh:
            fh.write("[General]\nFoo=1\n\n[Basic]\nProfile=%s\nProfileDir=%s\n" % (profile, profile))
    with open(os.path.join(cfg, "basic", "profiles", profile, "basic.ini"), "w") as fh:
        fh.write(basic_ini)
    return cfg


def test_record_dir_is_read_from_the_active_obs_profile_advanced_mode():
    with tempfile.TemporaryDirectory() as t:
        rec = os.path.join(t, "srv_REC")
        os.mkdir(rec)
        _mk(rec, "2026-01-01 10-00-00.mkv", 10, 400 * 86400, 1800000000)
        cfg = _obs_cfg(t, "strih-lx",
                       "[General]\nName=strih-lx\n\n[Output]\nMode=Advanced\n\n"
                       "[SimpleOutput]\nFilePath=/wrong/simple\n\n"
                       "[AdvOut]\nRecType=Standard\nRecFilePath=%s\nFFFilePath=/wrong/ff\n" % rec)
        r = _run(["--local-sweep", "--obs-config-dir", cfg, "--keep-runs", "0", "--keep-days", "0"],
                 env=_env({"RETENTION_NOW_EPOCH": "1800000000"}))
        assert r.returncode == 0, r.stdout + r.stderr
        assert rec in r.stdout and "OBS profile" in r.stdout
        assert "2026-01-01 10-00-00.mkv" in r.stdout


def test_record_dir_simple_mode_and_ffmpeg_rectype():
    with tempfile.TemporaryDirectory() as t:
        simple = os.path.join(t, "simple")
        ff = os.path.join(t, "ff")
        os.mkdir(simple)
        os.mkdir(ff)
        cfg = _obs_cfg(t, "p1", "[Output]\nMode=Simple\n\n[SimpleOutput]\nFilePath=%s\r\n"
                                "\n[AdvOut]\nRecFilePath=/wrong\n" % simple)
        r = _run(["--local-sweep", "--obs-config-dir", cfg])
        assert r.returncode == 0 and simple in r.stdout, r.stdout + r.stderr
    with tempfile.TemporaryDirectory() as t:
        ff = os.path.join(t, "ff")
        os.mkdir(ff)
        cfg = _obs_cfg(t, "p2", "[Output]\nMode=Advanced\n\n[AdvOut]\nRecType=FFmpeg\n"
                                "RecFilePath=/wrong\nFFFilePath=%s\n" % ff)
        r = _run(["--local-sweep", "--obs-config-dir", cfg])
        assert r.returncode == 0 and ff in r.stdout, r.stdout + r.stderr


def test_unresolvable_obs_profile_fails_loud_never_a_guess():
    with tempfile.TemporaryDirectory() as t:
        r = _run(["--local-sweep", "--obs-config-dir", os.path.join(t, "none")])
        assert r.returncode == 1, r.stdout + r.stderr
        assert "OBS profile" in r.stderr
        cfg = _obs_cfg(t, "p", "[Output]\nMode=Advanced\n\n[AdvOut]\nRecType=Standard\n")
        r = _run(["--local-sweep", "--obs-config-dir", cfg])
        assert r.returncode == 1, r.stdout + r.stderr
        assert "RecFilePath" in r.stderr


# --- the --box fleet dispatch -------------------------------------------------------------------

def _fake_sshpass(tmp):
    """A fake sshpass on PATH: logs its argv, the first stdin line and the stdin size."""
    bindir = os.path.join(tmp, "bin")
    os.makedirs(bindir)
    log = os.path.join(tmp, "calls.log")
    path = os.path.join(bindir, "sshpass")
    with open(path, "w") as f:
        f.write(
            "#!/usr/bin/env bash\n"
            'printf "ARGV %s\\n" "$*" >> "' + log + '"\n'
            'body="$(cat)"\n'
            'printf "STDIN_BYTES %s\\n" "${#body}" >> "' + log + '"\n'
            'printf "STDIN_HEAD %s\\n" "$(printf "%s" "$body" | head -1)" >> "' + log + '"\n'
            "exit 0\n"
        )
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC)
    env = _env()
    env["PATH"] = bindir + os.pathsep + env.get("PATH", "")
    return env, log


def _calls(log):
    return open(log).read() if os.path.exists(log) else ""


def test_box_strih_lx_runs_the_bash_local_sweep_over_ssh():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(["--box", "strih-lx"], env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _calls(log)
        assert "newlevel@10.77.9.202" in calls, calls
        assert "bash -s -- --local-sweep" in calls, calls
        assert "--execute" not in calls, "dry-run by default"
        assert "--record-dir" not in calls, "the box resolves its own record dir from its OBS profile"
        assert "powershell" not in calls and "scp" not in calls, calls
        assert "sudo" not in calls, "the record dir is newlevel-owned; no sudo"
        # the program fed over stdin is THIS script
        assert "STDIN_HEAD #!/usr/bin/env bash" in calls, calls


def test_box_strih_lx_passes_policy_record_dir_and_execute_through():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(["--box", "strih-lx", "--record-dir", "/srv/_REC", "--keep-runs", "20",
                  "--keep-days", "3", "--execute"], env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _calls(log)
        assert "--record-dir /srv/_REC" in calls, calls
        assert "--keep-runs 20 --keep-days 3" in calls, calls
        assert "--execute" in calls, calls


def test_box_stream_uses_the_windows_ps1_driver():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(["--box", "stream", "--record-dir", "C:\\Users\\newlevel\\Videos"], env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _calls(log)
        assert "scp -O" in calls and "newlevel@10.77.9.204:" in calls, calls
        assert "powershell -NoProfile -ExecutionPolicy Bypass -File" in calls, calls
        assert "bash -s" not in calls, calls


def test_box_retired_strih_and_unknown_box_are_usage_errors():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(["--box", "strih"], env=env)
        assert r.returncode == 2 and "RETIRED" in r.stderr and "--box strih-lx" in r.stderr, r.stderr
        r = _run(["--box", "nosuchbox"], env=env)
        assert r.returncode == 2 and "not in the fleet list" in r.stderr, r.stderr
        assert _calls(log) == ""


def test_host_still_refuses_the_linux_strih_and_points_at_box():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(["--host", "10.77.9.202"], env=env)
        assert r.returncode == 2, r.stdout + r.stderr
        assert "linux-genlock" in r.stderr and "--box strih-lx" in r.stderr, r.stderr
        assert _calls(log) == ""


# --- review round 1 -----------------------------------------------------------------------------

def test_out_of_range_policy_is_refused_never_a_silent_delete():
    # An over-range number used to break a `[ -lt ]` test INSIDE the plan's command substitution,
    # where bash drops -e: the row fell through to DELETE and the run still exited 0.
    now = 1800000000
    with tempfile.TemporaryDirectory() as d:
        _mk(d, "2026-01-01 10-00-00.mkv", 10, 400 * 86400, now)
        for args in (["--keep-days", "1000000000000000"], ["--keep-days", "36501"],
                     ["--keep-runs", "18446744073709551617"], ["--keep-runs", "1234567890"]):
            r = _run(["--local-sweep", "--record-dir", d] + args,
                     env=_env({"RETENTION_NOW_EPOCH": str(now)}))
            assert r.returncode == 2, (args, r.stdout + r.stderr)
            assert "DELETE" not in r.stdout, (args, r.stdout)
        r = _run(["--local-sweep", "--record-dir", d, "--keep-runs", "0", "--keep-days", "36500",
                  "--plan-tsv"], env=_env({"RETENTION_NOW_EPOCH": str(now)}))
        assert r.returncode == 0, r.stdout + r.stderr
        assert _plan_rows(r.stdout) == ([("within-days", "2026-01-01 10-00-00.mkv")], []), r.stdout


def test_bad_now_seam_fails_loud():
    with tempfile.TemporaryDirectory() as d:
        r = _run(["--local-sweep", "--record-dir", d],
                 env=_env({"RETENTION_NOW_EPOCH": "99999999999999999999999"}))
        assert r.returncode == 2, r.stdout + r.stderr


def _exec_sshpass(tmp):
    """A fake sshpass that RUNS the remote command the way the box would: the argument after
    user@host is the remote command string, evaluated by bash with the forwarded stdin as the
    program -- so the real `bash -s -- <printf %q args>` path executes end to end."""
    bindir = os.path.join(tmp, "bin")
    work = os.path.join(tmp, "remote-home")
    os.makedirs(bindir)
    os.makedirs(work)
    log = os.path.join(tmp, "calls.log")
    path = os.path.join(bindir, "sshpass")
    with open(path, "w") as f:
        f.write(
            "#!/usr/bin/env bash\n"
            'printf "ARGV %s\\n" "$*" >> "' + log + '"\n'
            'seen=0; remote=""\n'
            'for a in "$@"; do if [ "$seen" = 1 ]; then remote="$a"; fi; '
            'case "$a" in *@*) seen=1 ;; esac; done\n'
            'cat > "' + work + '/prog.sh"\n'
            'printf "STDIN_BYTES %s\\n" "$(wc -c < "' + work + '/prog.sh")" >> "' + log + '"\n'
            'cd "' + work + '"\n'
            'eval "$remote" < "' + work + '/prog.sh"\n'
        )
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC)
    env = _env({"HOME": work})
    env["PATH"] = bindir + os.pathsep + env.get("PATH", "")
    return env, log


def test_box_strih_lx_end_to_end_through_the_real_bash_s_path():
    now = 1800000000
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _exec_sshpass(tmp)
        env["RETENTION_NOW_EPOCH"] = str(now)
        rec = os.path.join(tmp, "srv _REC")  # a space in the path: the %q round trip must hold
        os.mkdir(rec)
        _mk(rec, "2026-01-01 10-00-00.mkv", 10, 30 * 86400, now)
        _mk(rec, "2026-01-02 10-00-00.mkv", 10, 1 * 86400, now)
        _mk(rec, "strih700105.mkv", 10, 900 * 86400, now)
        r = _run(["--box", "strih-lx", "--record-dir", rec, "--keep-runs", "1", "--keep-days", "0"],
                 env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _calls(log)
        assert "STDIN_BYTES %d" % os.path.getsize(REC) in calls, calls
        assert "DRY-RUN" in r.stdout and rec in r.stdout, r.stdout
        assert re.search(r"DELETE .*2026-01-01 10-00-00\.mkv", r.stdout), r.stdout
        assert len(os.listdir(rec)) == 3
        r = _run(["--box", "strih-lx", "--record-dir", rec, "--keep-runs", "1", "--keep-days", "0",
                  "--execute"], env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        assert sorted(os.listdir(rec)) == ["2026-01-02 10-00-00.mkv", "strih700105.mkv"]


def test_box_strih_lx_forwards_obs_config_dir_and_user():
    now = 1800000000
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _exec_sshpass(tmp)
        env["RETENTION_NOW_EPOCH"] = str(now)
        rec = os.path.join(tmp, "rec")
        os.mkdir(rec)
        _mk(rec, "2026-01-01 10-00-00.mkv", 10, 30 * 86400, now)
        cfg = _obs_cfg(tmp, "strih-lx", "[Output]\nMode=Advanced\n\n[AdvOut]\nRecFilePath=%s\n" % rec)
        r = _run(["--box", "strih-lx", "--obs-config-dir", cfg, "--user", "opuser"], env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _calls(log)
        assert "opuser@10.77.9.202" in calls and "--obs-config-dir" in calls, calls
        assert rec in r.stdout and "OBS profile" in r.stdout, r.stdout


def test_linux_leg_ssh_is_hardened_and_bounded():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(["--box", "strih-lx"], env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _calls(log)
        # 10.77.9.202 used to be the Windows strih: a stale known_hosts key must not block the leg.
        assert "UserKnownHostsFile=/dev/null" in calls and "LogLevel=ERROR" in calls, calls
        # timeout sits INSIDE sshpass (sshpass stays the outer, stubbable command).
        assert re.search(r"ARGV -p \S+ timeout [0-9]+ ssh ", calls), calls


def test_linux_leg_refuses_windows_only_flags():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        for args in (["--budget-gb", "10"], ["--remote-path", "C:\\x.ps1"]):
            r = _run(["--box", "strih-lx"] + args, env=env)
            assert r.returncode == 2, (args, r.stdout + r.stderr)
            assert "Windows" in r.stderr, r.stderr
        assert _calls(log) == ""


def test_non_regular_entries_are_reported_and_never_touched():
    now = 1800000000
    with tempfile.TemporaryDirectory() as t:
        d = os.path.join(t, "rec")
        os.mkdir(d)
        _mk(d, "2026-01-01 10-00-00.mkv", 10, 30 * 86400, now)
        _mk(d, "2026-01-02 10-00-00.mkv", 10, 1 * 86400, now)
        os.mkdir(os.path.join(d, "2026-01-03 10-00-00.mkv"))
        target = _mk(t, "outside.mkv", 10, 30 * 86400, now)
        os.symlink(target, os.path.join(d, "2026-01-04 10-00-00.mkv"))
        r = _run(["--local-sweep", "--record-dir", d, "--keep-runs", "1", "--keep-days", "0"],
                 env=_env({"RETENTION_NOW_EPOCH": str(now)}))
        assert r.returncode == 0, r.stdout + r.stderr
        assert r.stdout.count("not a regular file") == 2, r.stdout
        r = _run(["--local-sweep", "--record-dir", d, "--keep-runs", "1", "--keep-days", "0",
                  "--plan-tsv"], env=_env({"RETENTION_NOW_EPOCH": str(now)}))
        others = sorted(ln.split("\t")[-1] for ln in r.stdout.splitlines() if ln.startswith("OTHER\t"))
        assert others == ["2026-01-03 10-00-00.mkv", "2026-01-04 10-00-00.mkv"], r.stdout
        r = _run(["--local-sweep", "--record-dir", d, "--keep-runs", "1", "--keep-days", "0",
                  "--execute"], env=_env({"RETENTION_NOW_EPOCH": str(now)}))
        assert r.returncode == 0, r.stdout + r.stderr
        assert sorted(os.listdir(d)) == ["2026-01-02 10-00-00.mkv", "2026-01-03 10-00-00.mkv",
                                         "2026-01-04 10-00-00.mkv"]
        assert os.path.exists(target)


def test_unreadable_record_dir_fails_loud_not_an_empty_sweep():
    with tempfile.TemporaryDirectory() as t:
        d = os.path.join(t, "rec")
        os.mkdir(d)
        _mk(d, "2026-01-01 10-00-00.mkv", 10, 30 * 86400, 1800000000)
        os.chmod(d, 0)
        try:
            r = _run(["--local-sweep", "--record-dir", d])
        finally:
            os.chmod(d, 0o755)
        assert r.returncode == 1, r.stdout + r.stderr
        assert "not readable" in r.stderr, r.stderr


def test_help_prints_the_whole_header_including_env():
    r = _run(["--help"])
    assert r.returncode == 0
    assert "LINUX_BOX_USER" in r.stdout and "STRIH_SSH_PW" in r.stdout, r.stdout


# --- review round 2 -----------------------------------------------------------------------------

def test_help_prints_only_the_comment_header():
    r = _run(["--help"])
    assert r.returncode == 0
    assert "set -euo pipefail" not in r.stdout and "shopt" not in r.stdout, r.stdout
    assert not any(ln.startswith("#") for ln in r.stdout.splitlines()), r.stdout


def test_zero_ssh_timeout_is_refused_it_would_disable_the_bound():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        env["RETENTION_SSH_TIMEOUT"] = "0"
        r = _run(["--box", "strih-lx"], env=env)
        assert r.returncode == 2, r.stdout + r.stderr
        assert _calls(log) == ""


def test_flags_that_do_not_apply_to_a_mode_are_refused_never_ignored():
    with tempfile.TemporaryDirectory() as d:
        for args in (["--user", "x"], ["--budget-gb", "10"], ["--remote-path", "C:\\x.ps1"]):
            r = _run(["--local-sweep", "--record-dir", d] + args)
            assert r.returncode == 2, (args, r.stdout + r.stderr)
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(["--host", "10.77.9.204", "--obs-config-dir", "/x"], env=env)
        assert r.returncode == 2, r.stdout + r.stderr
        r = _run(["--host", "10.77.9.204", "--plan-tsv"], env=env)
        assert r.returncode == 2, r.stdout + r.stderr
        # print-only view + delete is refused on dev1, before any ssh.
        r = _run(["--box", "strih-lx", "--plan-tsv", "--execute"], env=env)
        assert r.returncode == 2, r.stdout + r.stderr
        assert _calls(log) == ""


# --- review round 3 -----------------------------------------------------------------------------

def test_box_linux_plan_tsv_stdout_is_pure_tsv():
    now = 1800000000
    with tempfile.TemporaryDirectory() as tmp:
        env, _log = _exec_sshpass(tmp)
        env["RETENTION_NOW_EPOCH"] = str(now)
        rec = os.path.join(tmp, "rec")
        os.mkdir(rec)
        _mk(rec, "2026-01-01 10-00-00.mkv", 10, 30 * 86400, now)
        _mk(rec, "2026-01-02 10-00-00.mkv", 10, 1 * 86400, now)
        r = _run(["--box", "strih-lx", "--record-dir", rec, "--keep-runs", "1", "--keep-days", "0",
                  "--plan-tsv"], env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        lines = r.stdout.splitlines()
        assert lines and all(ln.split("\t")[0] in ("OTHER", "PROTECT", "KEEP", "DELETE")
                             for ln in lines), r.stdout
        assert _plan_rows(r.stdout) == ([("newest-run", "2026-01-02 10-00-00.mkv")],
                                        ["2026-01-01 10-00-00.mkv"]), r.stdout
        assert "[strih-lx] ssh" in r.stderr, r.stderr
