#!/usr/bin/env python3
"""Issue 1415 -- every provisioning flow sets the rig time zone (Europe/Bratislava), every verifier
FAILs a box on any other zone.

Owner 12.10.2026: "vec s casovou zonou je najvacsia blbost, uz si nahadzoval kopec camboxov, a kopec
strih/imag nb, rovnaku chybu si mal vzdy". No flow set a zone, so every fresh box ran Etc/UTC and the
operator's browser-source clocks ran 2 h behind.

The design (main, Approach 1): ONE lib, scripts/lib/rig-timezone.sh. Its writer writes the
/etc/localtime symlink + /etc/timezone directly (timedatectl set-timezone fails on a read-only-root
cambox even inside an rw window, and timedatectl's read-back then lies), and every reader reads
`readlink /etc/localtime`. setup-device.sh STEP 1 and create-usb-linux.sh call it for the camboxes,
the shared OBS-box baseline item obs_box_timezone for setup-strih.sh / setup-imag.sh step 1;
verify-device.sh (as) and the baseline grader's `timezone` row grade it.

Tier-0: stdlib + bash only, every write into tmp_path, no rig, no network. Values reach bash as
ARGUMENTS, never inside the script text (the issue-1302 harness rule).
"""
import os
import re
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "rig-timezone.sh")
KIOSK = os.path.join(REPO, "scripts", "lib", "obs-box-kiosk.sh")
VERIFY_LIB = os.path.join(REPO, "scripts", "lib", "obs-box-baseline-verify.sh")
SETUP_DEVICE = os.path.join(REPO, "scripts", "setup-device.sh")
CREATE_USB = os.path.join(REPO, "scripts", "create-usb-linux.sh")
SETUP_STRIH = os.path.join(REPO, "scripts", "setup-strih.sh")
SETUP_IMAG = os.path.join(REPO, "scripts", "setup-imag.sh")
VERIFY_DEVICE = os.path.join(REPO, "scripts", "verify-device.sh")

ZONE = "Europe/Bratislava"
TARGET = "/usr/share/zoneinfo/Europe/Bratislava"


def _read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def _code(text):
    """The non-comment lines of a shell text."""
    return "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))


def _bash(script, *args, env=None):
    return subprocess.run(["bash", "-c", script, "harness", *args], capture_output=True, text=True,
                          env={**os.environ, **(env or {})}, timeout=60)


def _lib(snippet, *args, env=None):
    """Source the lib under the callers' strict mode and run SNIPPET ($1.. = ARGS)."""
    return _bash(f'set -euo pipefail\n. "{LIB}"\n{snippet}', *args, env=env)


def _fake_root(tmp_path, zones=(ZONE,), localtime="/usr/share/zoneinfo/Etc/UTC", timezone="Etc/UTC\n"):
    """A root with zoneinfo files for ZONES and /etc on UTC (LOCALTIME None = no /etc/localtime)."""
    root = tmp_path / "root"
    for z in zones:
        f = root / "usr" / "share" / "zoneinfo" / z
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(b"TZif2-fake")
    (root / "etc").mkdir(parents=True, exist_ok=True)
    if localtime is not None:
        os.symlink(localtime, root / "etc" / "localtime")
    if timezone is not None:
        (root / "etc" / "timezone").write_text(timezone)
    return root


# =================================================================================================
# 1. the lib: the one declaration, the writer, the reader, the verdict
# =================================================================================================

def test_the_lib_declares_bratislava_and_sources_silently():
    r = _lib('printf "%s" "$RIG_TIMEZONE"', env={"RIG_TIMEZONE": ""})
    assert r.returncode == 0 and r.stdout == ZONE and r.stderr == "", (r.stdout, r.stderr)


def test_apply_points_localtime_at_the_zone_and_writes_etc_timezone(tmp_path):
    root = _fake_root(tmp_path)
    r = _lib('rig_timezone_apply "$1"', str(root))
    assert r.returncode == 0, r.stdout + r.stderr
    assert os.readlink(root / "etc" / "localtime") == TARGET
    assert (root / "etc" / "timezone").read_text() == ZONE + "\n"
    assert "Europe/Bratislava" in r.stdout


def test_apply_replaces_a_copied_localtime_file_and_a_missing_etc_timezone(tmp_path):
    root = _fake_root(tmp_path, localtime=None, timezone=None)
    (root / "etc" / "localtime").write_bytes(b"TZif2-utc-copy")
    r = _lib('rig_timezone_apply "$1"', str(root))
    assert r.returncode == 0, r.stdout + r.stderr
    assert os.path.islink(root / "etc" / "localtime")
    assert os.readlink(root / "etc" / "localtime") == TARGET
    assert (root / "etc" / "timezone").read_text() == ZONE + "\n"


def test_apply_is_idempotent_and_leaves_a_box_already_on_the_zone_alone(tmp_path):
    root = _fake_root(tmp_path, localtime=TARGET, timezone=ZONE + "\n")
    before = os.lstat(root / "etc" / "localtime").st_ino
    r = _lib('rig_timezone_apply "$1"', str(root))
    assert r.returncode == 0, r.stdout + r.stderr
    assert "already" in r.stdout
    assert os.lstat(root / "etc" / "localtime").st_ino == before


def test_apply_fails_loud_and_writes_nothing_without_the_zoneinfo_file(tmp_path):
    root = _fake_root(tmp_path, zones=())
    r = _lib('rig_timezone_apply "$1"', str(root))
    assert r.returncode != 0
    assert "tzdata" in r.stderr and "zoneinfo/Europe/Bratislava" in r.stderr, r.stderr
    assert os.readlink(root / "etc" / "localtime") == "/usr/share/zoneinfo/Etc/UTC"
    assert (root / "etc" / "timezone").read_text() == "Etc/UTC\n"


def test_apply_refuses_a_zone_that_is_not_a_zoneinfo_name(tmp_path):
    root = _fake_root(tmp_path, zones=(ZONE, "Europe/Prague"))
    for bad in ("../../../etc/passwd", "/etc/passwd", "Europe/../../x", "Europe/Prague ",
                "Europe/Prague\nX", "Europe//Prague", "Europe/Prague/", "Europe/Pr\u00e1gue"):
        r = _lib('rig_timezone_apply "$1"', str(root), env={"RIG_TIMEZONE": bad})
        assert r.returncode != 0, bad
        assert "not a zoneinfo name" in r.stderr, (bad, r.stderr)
        assert os.readlink(root / "etc" / "localtime") == "/usr/share/zoneinfo/Etc/UTC", bad


def test_the_zone_override_is_the_one_knob(tmp_path):
    root = _fake_root(tmp_path, zones=("Europe/Prague",))
    r = _lib('rig_timezone_apply "$1"', str(root), env={"RIG_TIMEZONE": "Europe/Prague"})
    assert r.returncode == 0, r.stdout + r.stderr
    assert os.readlink(root / "etc" / "localtime") == "/usr/share/zoneinfo/Europe/Prague"
    r = _lib('rig_timezone_verdict Europe/Prague', env={"RIG_TIMEZONE": "Europe/Prague"})
    assert r.returncode == 0 and r.stdout.strip() == "OK"


def test_read_follows_the_link_never_timedatectl(tmp_path):
    for i, (link, want) in enumerate(((TARGET, ZONE), ("../usr/share/zoneinfo/Europe/Bratislava", ZONE),
                                      ("/usr/share/zoneinfo/Etc/UTC", "Etc/UTC"), ("/opt/odd", "/opt/odd"))):
        root = _fake_root(tmp_path / f"case{i}", localtime=link)
        r = _lib('rig_timezone_read "$1"', str(root))
        assert r.returncode == 0 and r.stdout == want + "\n", (link, r.stdout, r.stderr)
    root = _fake_root(tmp_path / "copied", localtime=None)
    (root / "etc" / "localtime").write_bytes(b"TZif2")
    r = _lib('rig_timezone_read "$1"', str(root))
    assert r.returncode == 0 and r.stdout == "\n"
    r = _lib('rig_timezone_read "$1"', str(tmp_path / "nothing-here"))
    assert r.returncode == 0 and r.stdout == "\n"
    assert "timedatectl" not in _code(_read(LIB))


def test_the_gather_cmd_is_self_contained_and_reads_the_real_localtime():
    r = _lib("rig_timezone_gather_cmd")
    assert r.returncode == 0, r.stderr
    gather = r.stdout
    assert "readlink" in gather and "timedatectl" not in gather
    # A fresh bash with NO lib sourced (the cambox has no checkout) runs it.
    g = subprocess.run(["bash", "-c", gather], capture_output=True, text=True, timeout=30)
    assert g.returncode == 0, g.stderr
    try:
        link = os.readlink("/etc/localtime")
    except OSError:
        link = ""
    want = link.split("/zoneinfo/", 1)[1] if "/zoneinfo/" in link else link
    assert g.stdout == want + "\n", (g.stdout, link)


def test_verdict_passes_only_the_rig_zone():
    r = _lib('rig_timezone_verdict "$1"', ZONE)
    assert r.returncode == 0 and r.stdout == "OK\n"
    r = _bash(f'. "{LIB}"\nrig_timezone_verdict "$1"; echo "rc=$?"', "Etc/UTC")
    assert "FAIL:" in r.stdout and "'Etc/UTC'" in r.stdout and ZONE in r.stdout and "rc=1" in r.stdout
    r = _bash(f'. "{LIB}"\nrig_timezone_verdict ""; echo "rc=$?"')
    assert "FAIL:" in r.stdout and "no zoneinfo symlink" in r.stdout and "rc=1" in r.stdout


# =================================================================================================
# 2. the cambox flows: setup-device.sh STEP 1 + create-usb-linux.sh
# =================================================================================================

def _step1(text):
    s = text.find("# STEP 1: Set hostname")
    e = text.find("# STEP 2:", s)
    assert 0 <= s < e, "setup-device.sh keeps its STEP 1 hostname block"
    return text[s:e]


def test_setup_device_sources_the_lib_and_applies_it_in_step1_inside_the_rw_window():
    text = _read(SETUP_DEVICE)
    assert re.search(r'(?m)^\. "\$HERE/lib/rig-timezone\.sh"', text)
    code = _code(_step1(text))
    assert re.search(r"(?m)^rig_timezone_apply \|\| fail ", code), code
    # the root is writable before STEP 1 (an in-place re-run on a read-only appliance)
    flow = _code(text)
    # the call line, not the source line's trailing comment that names the function
    assert 0 <= flow.find("\nensure_root_writable\n") < flow.find("\nrig_timezone_apply || fail ")
    assert "set-timezone" not in _code(text)


def test_setup_device_step1_installs_tzdata_when_the_zoneinfo_file_is_missing():
    code = _code(_step1(_read(SETUP_DEVICE)))
    install = code.find("tzdata")
    assert 0 <= install < code.find("rig_timezone_apply"), code
    assert "rig_timezone_zoneinfo" in code


def test_create_usb_installs_tzdata_in_the_chroot_and_writes_the_zone_into_the_image(tmp_path):
    text = _read(CREATE_USB)
    assert re.search(r'(?m)^\. "\$SCRIPT_DIR/lib/rig-timezone\.sh"', text)
    setup = text[text.find("cat > \"$MOUNT_ROOT/tmp/setup.sh\" << 'SETUP_EOF'"):text.find("\nSETUP_EOF\n")]
    first_install = setup[setup.find("apt-get install -y"):]
    first_install = first_install[:first_install.find("\n\n")]
    assert re.search(r"(?m)^\s+tzdata\b", first_install), first_install
    code = _code(text)
    run = code.find('chroot "$MOUNT_ROOT" /tmp/setup.sh')
    apply_line = [ln for ln in code.splitlines() if ln.strip().startswith("rig_timezone_apply")]
    assert len(apply_line) == 1 and '"$MOUNT_ROOT"' in apply_line[0] and "|| error" in apply_line[0]
    assert 0 <= run < code.find(apply_line[0])
    # run the line itself against a fake image root
    root = _fake_root(tmp_path)
    r = _bash(f'set -euo pipefail\n. "{LIB}"\nerror() {{ echo "ERROR $1"; exit 1; }}\n'
              'MOUNT_ROOT="$1"\n' + apply_line[0].strip(), str(root))
    assert r.returncode == 0, r.stdout + r.stderr
    assert os.readlink(root / "etc" / "localtime") == TARGET
    r = _bash(f'set -euo pipefail\n. "{LIB}"\nerror() {{ echo "ERROR $1"; exit 1; }}\n'
              'MOUNT_ROOT="$1"\n' + apply_line[0].strip(), str(_fake_root(tmp_path / "bare", zones=())))
    assert r.returncode == 1 and "ERROR" in r.stdout and "issue 1415" in r.stdout


# =================================================================================================
# 3. the Linux OBS boxes: obs_box_timezone in the shared baseline, step 1 of both setup scripts
# =================================================================================================

def _obs_box_timezone(root, apt_installs_zone=False):
    """Run the real obs_box_timezone ROOT with apt stubbed (a function outranks the binary)."""
    plant = ('mkdir -p "$ROOT_UNDER_TEST/usr/share/zoneinfo/Europe"; '
             ': > "$ROOT_UNDER_TEST/usr/share/zoneinfo/Europe/Bratislava"; ') if apt_installs_zone else ""
    # The apt calls are logged to a FILE: the installer sends apt's own stdout to /dev/null.
    stub = ('fail() { echo "FAIL: $1" >&2; exit 1; }\nYELLOW=""; NC=""\n'
            'obs_box_apt_update() { echo "APT-UPDATE" >> "$APT_LOG"; }\n'
            'apt-get() { echo "APT-GET $*" >> "$APT_LOG"; ' + plant + '}\n')
    log = root.parent / "apt.log"
    log.write_text("")
    r = _bash(f'set -euo pipefail\n. "{VERIFY_LIB}"\n{stub}ROOT_UNDER_TEST="$1"; APT_LOG="$2"\n'
              'obs_box_timezone "$1"\necho "rc=$?"', str(root), str(log))
    return r, log.read_text()


def test_obs_box_timezone_writes_the_zone_through_the_lib(tmp_path):
    root = _fake_root(tmp_path)
    r, apt = _obs_box_timezone(root)
    assert r.returncode == 0 and "rc=0" in r.stdout, r.stdout + r.stderr
    assert apt == "", apt
    assert os.readlink(root / "etc" / "localtime") == TARGET


def test_obs_box_timezone_installs_tzdata_first_when_the_zoneinfo_file_is_missing(tmp_path):
    root = _fake_root(tmp_path, zones=())
    r, apt = _obs_box_timezone(root, apt_installs_zone=True)
    assert r.returncode == 0, r.stdout + r.stderr
    assert 0 <= apt.find("APT-UPDATE") < apt.find("APT-GET install -y tzdata"), apt
    assert "installing tzdata" in r.stdout
    assert os.readlink(root / "etc" / "localtime") == TARGET


def test_obs_box_timezone_fails_loud_when_no_zoneinfo_can_be_installed(tmp_path):
    root = _fake_root(tmp_path, zones=())
    r, _apt = _obs_box_timezone(root, apt_installs_zone=False)
    assert r.returncode == 1 and "FAIL: issue 1415" in r.stderr, r.stdout + r.stderr
    assert os.readlink(root / "etc" / "localtime") == "/usr/share/zoneinfo/Etc/UTC"


def _step(text, n, nxt):
    s = text.find(f'\nstep {n} "')
    e = text.find(f'\nstep {nxt}', s + 1)
    assert 0 <= s < e, (n, nxt)
    return text[s:e]


def test_both_obs_box_setup_scripts_set_the_zone_in_step1():
    for path, nxt in ((SETUP_STRIH, '"1b"'), (SETUP_IMAG, '2 "')):
        code = _code(_step(_read(path), 1, nxt))
        assert re.search(r"(?m)^obs_box_timezone\s*$", code), (path, code)
        # setup-imag keeps its legit `timedatectl set-ntp false` (dantesync owns the clock); only a
        # zone write through timedatectl is banned
        assert "set-timezone" not in _code(_read(path)), path


def test_the_kiosk_half_sources_the_lib_and_never_uses_timedatectl():
    text = _read(KIOSK)
    assert re.search(r'rig-timezone\.sh', _code(text))
    assert "timedatectl" not in _code(text)


# =================================================================================================
# 4. the graders: the baseline `timezone` row + verify-device (as)
# =================================================================================================

def _baseline_row(facts, item):
    r = _bash(f'. "{VERIFY_LIB}"\nobs_box_baseline_verdict <<<"$1"', facts)
    for line in r.stdout.splitlines():
        name, st, detail = (line.split("|", 2) + ["", ""])[:3]
        if name == item:
            return st, detail
    raise AssertionError(f"no {item} row: {r.stdout}")


def test_the_baseline_grader_grades_the_zone_first_in_provisioning_order():
    st, detail = _baseline_row(f"timezone={ZONE}\ngather_done=1\n", "timezone")
    assert st == "OK", detail
    st, detail = _baseline_row("timezone=Etc/UTC\ngather_done=1\n", "timezone")
    assert st == "FAIL" and "Etc/UTC" in detail, detail
    st, detail = _baseline_row("timezone=\ngather_done=1\n", "timezone")
    assert st == "FAIL", detail
    st, detail = _baseline_row("timezone=Europe/Bratislava\n", "timezone")
    assert st == "FAIL" and "did not complete" in detail
    r = _bash(f'. "{VERIFY_LIB}"\nobs_box_baseline_verdict <<<"$1"', f"timezone={ZONE}\ngather_done=1\n")
    assert r.stdout.splitlines()[0].startswith("timezone|"), r.stdout


def test_the_baseline_gather_reads_the_link_through_the_lib():
    r = _bash(f'. "{VERIFY_LIB}"\nbash -c "$(obs_box_baseline_gather_snippet nosuchbox nosuchuser nosuch.service)"')
    assert r.returncode == 0, r.stderr
    try:
        link = os.readlink("/etc/localtime")
    except OSError:
        link = ""
    want = link.split("/zoneinfo/", 1)[1] if "/zoneinfo/" in link else link
    assert f"\ntimezone={want}\n" in "\n" + r.stdout, r.stdout


def _as_block(text):
    s = text.find("\n# (as) ")
    e = text.find("\n# (ar) ", s)
    assert 0 <= s < e, "(as) sits right before (ar)"
    return text[s:e]


def test_verify_device_documents_sources_and_places_as():
    text = _read(VERIFY_DEVICE)
    header = text[:text.find("set -euo pipefail")]
    assert "(as)" in header and "issue 1415" in header[header.find("(as)"):]
    usage = text[text.find("usage() {"):]
    assert "(as)" in usage[:usage.find("\nEOF\n")]
    assert re.search(r'(?m)^\. "\$HERE/lib/rig-timezone\.sh"', text)
    guard = text.find("never run the live SSH flow below.")
    at = text.find("\n# (as) ")
    q = text.rfind("# (q) .bak cruft drift")
    assert guard < text.find("\n# (ak) ") < at < text.find("\n# (ar) ") < text.find("\n# (al) ") < q
    # never inside the slices other tests EXECUTE: (ar)..(al), (aq)..(ao), (ao)..(an), (an)..(q)
    assert not (text.find("\n# (aq) ") < at < q)


def test_verify_device_as_is_a_hard_gate_over_the_shared_lib():
    code = _code(_as_block(_read(VERIFY_DEVICE)))
    assert "rig_timezone_gather_cmd" in code and "rig_timezone_verdict" in code
    assert 'fail "' in code and 'warn "' not in code and "timedatectl" not in code


def _run_as(ssh_out, ssh_rc=0):
    block = _as_block(_read(VERIFY_DEVICE))
    prelude = (f'. "{LIB}"\n'
               'ok() { echo "OK $1"; }\nfail() { echo "FAIL $1"; }\n'
               f'ssh_box() {{ printf "%s" "$SSH_OUT"; return {ssh_rc}; }}\n')
    r = _bash("set -euo pipefail\n" + prelude + block, env={"SSH_OUT": ssh_out})
    assert r.returncode == 0, r.stdout + r.stderr
    return r.stdout


def test_verify_device_as_passes_the_rig_zone_and_fails_utc_and_unreadable():
    out = _run_as(ZONE + "\n")
    assert out.startswith("OK ") and "FAIL" not in out and ZONE in out, out
    out = _run_as("Etc/UTC\n")
    assert out.startswith("FAIL ") and "'Etc/UTC'" in out, out
    out = _run_as("")
    assert out.startswith("FAIL "), out
    out = _run_as("", ssh_rc=255)
    assert out.startswith("FAIL ") and "rc=255" in out, out
