#!/usr/bin/env python3
"""Issue 1446 -- a new / reflashed cambox starts on the fleet kernel line, and verify-device says so.

The design (main, comment 6254503734, Approach 1): the base image that create-usb-linux.sh builds
installed `linux-image-generic` (the GA 6.8 line) and apt-held the GA metas, so every reflashed box
booted 6.8 -- off the line the 17-18.10.2026 moves put all seven boxes on (7.0 HWE +
`lowlatency-kernel`, preempt=full), and an N150 GPU (8086:46d4) has no i915 driver there. Nothing in
verify-device graded the kernel LINE: `(k)` checks one kernel, `(av)` only a bound GPU driver.

- create-usb-linux.sh installs `rt_kernel_flavour` (the ONE kernel decision in
  scripts/lib/rt-kernel-plan.sh, copied + sourced in the chroot) and holds the HWE line's metas.
- verify-device.sh `(aw)` "fleet kernel line" (HARD): the lowlatency meta installed (dpkg Status
  `install|hold ok installed`, the lib predicate `rt_kernel_pkg_installed`), the running kernel
  booted with preempt=full, and `uname -r` = the image the installed `linux-image-generic-hwe-24.04`
  meta depends on. The FAIL names `scripts/rt-kernel-upgrade.sh --box <ip>`, never setup-device.

Tier-0: stdlib + bash only, no rig, no network. Values reach bash as ARGUMENTS or env, never inside
the script text (the issue-1302 harness rule).
"""
import json
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "rt-kernel-plan.sh")
VERIFY_DEVICE = os.path.join(REPO, "scripts", "verify-device.sh")
CREATE_USB = os.path.join(REPO, "scripts", "create-usb-linux.sh")

FLAVOUR = "linux-lowlatency-hwe-24.04"
HWE_IMAGE_META = "linux-image-generic-hwe-24.04"
HWE_METAS = ["linux-lowlatency-hwe-24.04", "linux-image-generic-hwe-24.04",
             "linux-headers-generic-hwe-24.04", "lowlatency-kernel"]
GA_HOLD = "apt-mark hold linux-image-generic linux-headers-generic linux-generic"

K70 = "7.0.0-38-generic"
K68 = "6.8.0-142-generic"
# The archive's 7.0.0-38 HWE image meta Depends (apt-cache show, read on dev1 18.10.2026): the image
# is ONE entry among modules / zfs / v4l2loopback / firmware entries the parse must skip.
DEPS_70 = ("linux-image-7.0.0-38-generic, linux-modules-7.0.0-38-generic, "
           "linux-main-modules-zfs-7.0.0-38-generic, linux-main-modules-v4l2loopback-7.0.0-38-generic, "
           "linux-firmware, intel-microcode, amd64-microcode")
CMDLINE_LL = ("BOOT_IMAGE=/boot/vmlinuz-7.0.0-38-generic root=UUID=0b1c ro console=tty0 isolcpus=3 "
              "nohz_full=3 rcu_nocbs=3 irqaffinity=0-2 preempt=full rcu_nocbs=all")
CMDLINE_GA = ("BOOT_IMAGE=/boot/vmlinuz-6.8.0-142-generic root=UUID=0b1c ro console=tty0 isolcpus=3 "
              "nohz_full=3 rcu_nocbs=3 irqaffinity=0-2")


def _read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def _code(text):
    """The non-comment lines of a shell text."""
    return "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))


def _bash_env(env=None):
    e = dict(os.environ, **(env or {}))
    # A bracket range follows the locale (issue 1302): grade under en_US.UTF-8 when the box has it.
    if "en_US.utf8" in subprocess.run(["locale", "-a"], capture_output=True, text=True).stdout:
        e["LC_ALL"] = "en_US.utf8"
    return e


def _bash(script, *args, env=None):
    return subprocess.run(["bash", "-c", script, "harness", *args], capture_output=True, text=True,
                          env=_bash_env(env), timeout=60)


def _lib(snippet, *args, env=None):
    """Source the lib under the callers' strict mode and run SNIPPET ($1.. = ARGS)."""
    return _bash(f'set -euo pipefail\n. "{LIB}"\n{snippet}', *args, env=env)


def _block(kernel=K70, cmdline=CMDLINE_LL, flavour="install ok installed",
           hwe="install ok installed", deps=DEPS_70, end=True):
    rows = []
    if kernel is not None:
        rows.append(f"kernel={kernel}")
    if cmdline is not None:
        rows.append(f"cmdline={cmdline}")
    if flavour is not None:
        rows.append(f"flavour={flavour}")
    if hwe is not None:
        rows.append(f"hwe={hwe}")
    if deps is not None:
        rows.append(f"hwe_depends={deps}")
    if end:
        rows.append("end")
    return "\n".join(rows) + "\n"


def _verdict(block):
    """(rc, stdout) of rt_kernel_line_verdict over BLOCK, under the caller's strict mode."""
    r = _lib('rc=0; rt_kernel_line_verdict "$1" || rc=$?; echo "RC=$rc"', block)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "unbound variable" not in r.stderr, r.stderr
    out, _, rc = r.stdout.rpartition("RC=")
    return int(rc.strip()), out


def _fails(out):
    return [ln for ln in out.splitlines() if ln.startswith("FAIL: ")]


# --- the lib: one source for the kernel line ---------------------------------------------------

def test_the_lib_names_the_hwe_metas_create_usb_holds():
    r = _lib("rt_kernel_line_held_metas")
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == HWE_METAS, r.stdout
    r = _lib("rt_kernel_hwe_image_meta")
    assert r.returncode == 0 and r.stdout == HWE_IMAGE_META, r.stdout
    r = _lib("rt_kernel_flavour")
    assert r.stdout == FLAVOUR and FLAVOUR in HWE_METAS


# --- the verdict -------------------------------------------------------------------------------

def test_lowlatency_installed_preempt_full_and_the_hwe_image_running_passes():
    rc, out = _verdict(_block())
    assert rc == 0, out
    assert out.startswith("OK: ") and not _fails(out), out
    assert K70 in out and "preempt=full" in out and FLAVOUR in out, out


def test_a_held_meta_reads_installed():
    rc, out = _verdict(_block(flavour="hold ok installed", hwe="hold ok installed"))
    assert rc == 0 and out.startswith("OK: "), out


def test_the_ga_68_line_running_fails_on_every_axis():
    # a box built from an old stick: GA 6.8 running, no lowlatency meta, no preempt=full, no HWE meta
    rc, out = _verdict(_block(kernel=K68, cmdline=CMDLINE_GA, flavour="", hwe="", deps=""))
    assert rc != 0, out
    fails = _fails(out)
    assert not out.startswith("OK"), out
    assert any(FLAVOUR in f and "not installed" in f for f in fails), out
    assert any("preempt=full" in f and K68 in f for f in fails), out
    assert any(HWE_IMAGE_META in f for f in fails), out


def test_the_hwe_line_installed_but_68_still_booted_fails_naming_both_kernels():
    rc, out = _verdict(_block(kernel=K68, cmdline=CMDLINE_GA))
    assert rc != 0, out
    assert any(K68 in f and K70 in f for f in _fails(out)), out


def test_an_image_version_mismatch_fails_naming_both_versions():
    # a newer HWE image installed by the meta but not booted yet (or the box runs another 7.0)
    rc, out = _verdict(_block(kernel="7.0.0-30-generic"))
    assert rc != 0, out
    fails = _fails(out)
    assert len(fails) == 1, out
    assert "7.0.0-30-generic" in fails[0] and K70 in fails[0] and HWE_IMAGE_META in fails[0], out


def test_preempt_full_is_a_whole_word_never_a_substring():
    for cmdline in (CMDLINE_GA + " preempt=none", CMDLINE_GA + " xpreempt=full",
                    CMDLINE_GA + " preempt=fullx", CMDLINE_GA + " nopreempt=full", ""):
        rc, out = _verdict(_block(cmdline=cmdline))
        assert rc != 0 and any("preempt=full" in f for f in _fails(out)), (cmdline, out)
    rc, out = _verdict(_block(cmdline="preempt=full"))
    assert rc == 0, out


def test_a_removed_or_half_installed_meta_is_not_installed():
    for status in ("deinstall ok config-files", "install ok unpacked", "hold ok half-configured",
                   "unknown ok not-installed", "hold ok not-installed", ""):
        rc, out = _verdict(_block(flavour=status))
        assert rc != 0 and any(FLAVOUR in f for f in _fails(out)), (status, out)
        rc, out = _verdict(_block(hwe=status))
        assert rc != 0 and any(HWE_IMAGE_META in f for f in _fails(out)), (status, out)


def test_the_image_dependency_is_read_through_version_constraints_and_alternatives():
    for deps in ("linux-image-7.0.0-38-generic (= 7.0.0-38.38~24.04.4), linux-firmware",
                 "linux-firmware, linux-image-7.0.0-38-generic | linux-image-unsigned-7.0.0-38-generic",
                 "linux-image-7.0.0-38-generic"):
        rc, out = _verdict(_block(deps=deps))
        assert rc == 0, (deps, out)


def test_no_image_or_two_images_in_the_dependency_fails_as_unreadable():
    for deps in ("", "linux-firmware, intel-microcode",
                 "linux-image-7.0.0-38-generic, linux-image-7.0.0-30-generic",
                 "linux-image-generic, linux-firmware"):
        rc, out = _verdict(_block(deps=deps))
        assert rc != 0, (deps, out)
        assert any(HWE_IMAGE_META in f and "depends on" in f for f in _fails(out)), (deps, out)


def test_an_empty_cut_or_unreadable_read_fails_never_ok():
    for block in ("", "\n", "   \n", _block(end=False), _block(kernel=None), _block(kernel="")):
        rc, out = _verdict(block)
        assert rc != 0 and out.startswith("FAIL: "), (block, out)
        assert "OK" not in out, out


def test_a_missing_fact_row_fails():
    for missing in ("cmdline", "flavour", "hwe", "deps"):
        rc, out = _verdict(_block(**{missing: None}))
        assert rc != 0 and _fails(out), (missing, out)


# --- the gather (run locally against fake box tools) -------------------------------------------

FAKE_DPKG = r'''
import fnmatch, json, os, sys
db = json.load(open(os.environ["FAKE_DPKG_DB"]))
args = sys.argv[1:]
fmt, pats, i = None, [], 0
while i < len(args):
    a = args[i]
    if a.startswith("-f="):
        fmt = a[3:]
    elif a.startswith("--showformat="):
        fmt = a[len("--showformat="):]
    elif a == "-f":
        i += 1
        fmt = args[i]
    elif a != "-W":
        pats.append(a)
    i += 1
hit = False
for pat in pats:
    for name in sorted(db):
        if fnmatch.fnmatchcase(name, pat):
            hit = True
            row = db[name]
            out = fmt.replace("${Package}", name).replace("${Status}", row["status"])
            sys.stdout.write(out.replace("${Depends}", row.get("depends", "")).replace("\\n", "\n"))
if not hit:
    sys.stderr.write("dpkg-query: no packages found matching %s\n" % " ".join(pats))
sys.exit(0 if hit else 1)
'''


def _gather(tmp_path, db, running=K70, cmdline=CMDLINE_LL):
    """Run the lib's remote gather locally: fake dpkg-query + uname on PATH, CMDLINE in a file."""
    d = tmp_path / "bin"
    d.mkdir()
    (tmp_path / "db.json").write_text(json.dumps(db))
    (tmp_path / "cmdline").write_text(cmdline + "\n")
    dpkg = d / "dpkg-query"
    dpkg.write_text(f"#!{sys.executable}\n{FAKE_DPKG}")
    dpkg.chmod(0o755)
    uname = d / "uname"
    uname.write_text(f"#!/bin/sh\necho {running}\n")
    uname.chmod(0o755)
    snip = _lib('rt_kernel_line_gather_remote_snippet "$1"', str(tmp_path / "cmdline"))
    assert snip.returncode == 0, snip.stderr
    env = {"PATH": f"{d}:{os.environ['PATH']}", "FAKE_DPKG_DB": str(tmp_path / "db.json")}
    r = _bash("set -euo pipefail\n" + snip.stdout, env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    return r.stdout


def test_the_gather_reads_a_held_lowlatency_box_and_the_verdict_passes(tmp_path):
    db = {FLAVOUR: {"status": "hold ok installed"},
          HWE_IMAGE_META: {"status": "hold ok installed", "depends": DEPS_70},
          "lowlatency-kernel": {"status": "hold ok installed"}}
    out = _gather(tmp_path, db)
    assert out.splitlines()[0] == f"kernel={K70}" and out.splitlines()[-1] == "end", out
    assert f"hwe_depends={DEPS_70}" in out.splitlines(), out
    rc, v = _verdict(out)
    assert rc == 0 and v.startswith("OK: "), v


def test_the_gather_on_a_ga_box_with_no_hwe_packages_fails_the_verdict(tmp_path):
    db = {"linux-image-generic": {"status": "hold ok installed", "depends": "linux-image-6.8.0-142-generic"}}
    out = _gather(tmp_path, db, running=K68, cmdline=CMDLINE_GA)
    assert out.splitlines()[-1] == "end", out
    rc, v = _verdict(out)
    assert rc != 0 and len(_fails(v)) >= 3, v


def test_the_gather_on_this_machines_real_dpkg_parses(tmp_path):
    """The default paths (real dpkg-query, uname, /proc/cmdline): the read must parse. This runner
    may not be on the line, so the verdict is any named result, never a cut or unreadable one."""
    snip = _lib("rt_kernel_line_gather_remote_snippet")
    assert snip.returncode == 0, snip.stderr
    r = _bash("set -euo pipefail\n" + snip.stdout)
    assert r.returncode == 0, r.stdout + r.stderr
    lines = r.stdout.splitlines()
    assert lines[0] == f"kernel={os.uname().release}" and lines[-1] == "end", r.stdout
    rc, v = _verdict(r.stdout)
    assert "cut" not in v and "could not be read" not in v, v
    assert v.startswith("OK: ") or v.startswith("FAIL: "), v


def test_the_lib_stays_source_only_and_strict_mode_safe():
    text = _read(LIB)
    assert "set -euo pipefail" not in _code(text)
    r = _lib("true")
    assert r.returncode == 0 and r.stdout == "" and r.stderr == "", r


def test_no_bracket_range_on_a_regex_line_of_the_lib():
    for ln in _code(_read(LIB)).splitlines():
        if "=~" not in ln:
            continue
        regex = ln.split("=~", 1)[1].split("]]", 1)[0]
        for body in re.findall(r"\[([^\]]*)\]", regex):
            assert not re.search(r"\w-\w", body), ln


# --- verify-device (aw) ------------------------------------------------------------------------

def _aw_block(text):
    s = text.find("\n# (aw) ")
    e = text.find("\n# (", s + 10)
    assert 0 <= s < e, "(aw) block present"
    return text[s:e]


def test_verify_device_documents_sources_and_places_aw():
    text = _read(VERIFY_DEVICE)
    assert text.count("(aw) fleet kernel line (issue 1446)") == 3, "header, usage() and the check"
    header = text[:text.find("set -euo pipefail")]
    assert "(aw) fleet kernel line (issue 1446)" in header
    usage = text[text.find("usage() {"):]
    assert "(aw) fleet kernel line (issue 1446)" in usage[:usage.find("\nEOF\n")]
    assert re.search(r'(?m)^\. "\$HERE/lib/rt-kernel-plan\.sh"', text)
    guard = text.find("never run the live SSH flow below.")
    aw = text.find("\n# (aw) ")
    q = text.rfind("# (q) .bak cruft drift")
    # after (ak), before (av): outside every slice another test EXECUTES ((av)..(au), (au)..(at),
    # (at)..(as), (as)..(ar), (ar)..(al), (aq)..(ao), (ao)..(an), (an)..(q), (ap)..(af)), before (q)
    assert guard < text.find("\n# (ak) ") < aw < text.find("\n# (av) ") < text.find("\n# (au) ") < q
    # the block right after (aw) is (av), so the (ak) pytest's next-marker slice still ends at (aw)
    assert text.find("\n# (", aw + 10) == text.find("\n# (av) ")


def test_verify_device_aw_is_a_hard_gate_over_the_shared_lib():
    code = _code(_aw_block(_read(VERIFY_DEVICE)))
    assert "rt_kernel_line_gather_remote_snippet" in code and "rt_kernel_line_verdict" in code
    assert 'fail "' in code and 'warn "' not in code
    assert "scripts/rt-kernel-upgrade.sh --box" in code
    assert "setup-device" not in code.replace("never setup-device", "")


def _run_aw(ssh_out, ssh_rc=0, override=""):
    block = _aw_block(_read(VERIFY_DEVICE))
    prelude = (f'. "{LIB}"\nIP=10.77.9.65\n'
               'ok() { echo "OK $1"; }\nfail() { echo "FAIL $1"; }\n'
               'ssh_box() { printf "%s" "$SSH_OUT"; return "$SSH_RC"; }\n' + override)
    r = _bash("set -euo pipefail\n" + prelude + block, env={"SSH_OUT": ssh_out, "SSH_RC": str(ssh_rc)})
    assert r.returncode == 0, r.stdout + r.stderr
    return r.stdout


def test_verify_device_aw_passes_the_line_and_fails_68_mismatch_and_unreadable():
    out = _run_aw(_block())
    assert out.startswith("OK ") and "FAIL" not in out and K70 in out, out
    out = _run_aw(_block(flavour="hold ok installed", hwe="hold ok installed"))
    assert out.startswith("OK ") and "FAIL" not in out, out
    out = _run_aw(_block(kernel=K68, cmdline=CMDLINE_GA, flavour="", hwe="", deps=""))
    assert out.startswith("FAIL ") and "OK " not in out and K68 in out, out
    assert "scripts/rt-kernel-upgrade.sh --box 10.77.9.65" in out, out
    out = _run_aw(_block(kernel="7.0.0-30-generic"))
    assert out.startswith("FAIL ") and "7.0.0-30-generic" in out and K70 in out, out
    out = _run_aw("")
    assert out.startswith("FAIL ") and "OK " not in out, out
    out = _run_aw("", ssh_rc=255)
    assert out.startswith("FAIL ") and "rc=255" in out, out
    out = _run_aw(_block(end=False))
    assert out.startswith("FAIL ") and "OK " not in out, out


def test_verify_device_aw_fails_an_empty_verdict_never_silently_passes():
    out = _run_aw(_block(), override="rt_kernel_line_verdict() { return 1; }\n")
    assert out.startswith("FAIL ") and "verdict was empty" in out, out
    assert "OK " not in out, out


# --- create-usb-linux.sh: the base image installs + holds the fleet line -------------------------

def _chroot_setup(text):
    s = text.find("<< 'SETUP_EOF'")
    e = text.find("\nSETUP_EOF\n", s)
    assert 0 <= s < e, "the chroot setup.sh heredoc"
    return text[s:e]


def test_create_usb_installs_the_flavour_from_the_lib_never_the_ga_image():
    setup = _code(_chroot_setup(_read(CREATE_USB)))
    assert "source /tmp/rt-kernel-plan.sh" in setup
    assert 'KERNEL_FLAVOUR="$(rt_kernel_flavour)"' in setup
    install = setup[setup.find("apt-get install -y"):]
    install = install[:install.find("\n\n")]
    assert '"$KERNEL_FLAVOUR"' in install, install
    assert not re.search(r"(?m)^\s*linux-image-generic\b", setup), "the GA image is never installed"
    assert setup.find("source /tmp/rt-kernel-plan.sh") < setup.find('"$KERNEL_FLAVOUR"')


def test_create_usb_holds_the_hwe_metas_never_the_ga_ones():
    text = _read(CREATE_USB)
    setup = _code(_chroot_setup(text))
    assert "apt-mark hold $(rt_kernel_line_held_metas)" in setup
    assert GA_HOLD not in _code(text)
    # the #295 kernel guard stays: unattended upgrades off + the kernel blacklist + the saved default
    for kept in ("20auto-upgrades", 'APT::Periodic::Unattended-Upgrade "0"',
                 "51camera-box-no-kernel-autoupgrade", '"linux-image";', "GRUB_DEFAULT=saved",
                 "grub-set-default 0"):
        assert kept in setup, kept


def test_create_usb_verifies_the_line_inside_the_chroot():
    setup = _code(_chroot_setup(_read(CREATE_USB)))
    tail = setup[setup.find('echo "Verifying installation..."'):]
    assert 'rt_kernel_pkg_installed "$(dpkg-query -W -f=\'${Status}\' "$KERNEL_FLAVOUR"' in tail
    assert "preempt=full" in tail and "/boot/grub/grub.cfg" in tail


def test_create_usb_copies_the_lib_and_its_two_sourced_libs_and_checks_them_first():
    text = _read(CREATE_USB)
    code = _code(text)
    for lib in ("rt-kernel-plan.sh", "ro-window.sh", "ro-root.sh"):
        assert f'cp "$SCRIPT_DIR/lib/{lib}" "$MOUNT_ROOT/tmp/{lib}"' in code, lib
        assert f'"$MOUNT_ROOT/tmp/{lib}"' in code[code.find('rm -f "$MOUNT_ROOT/tmp/setup.sh"'):].split("\n", 1)[0], lib
    req = text[text.find("check_required_files() {"):]
    req = req[:req.find("\n}\n")]
    order = [req.find(f'"$SCRIPT_DIR/lib/{x}"') for x in
             ("install-grub-efi.sh", "camera-box-grow-root.sh", "grub-fast-boot.sh",
              "rt-kernel-plan.sh", "ro-window.sh", "ro-root.sh")]
    assert all(i >= 0 for i in order) and order == sorted(order), order


def test_the_copied_libs_alone_source_in_a_chroot_tmp_and_name_the_line(tmp_path):
    """Do what configure_system does: copy every `$SCRIPT_DIR/lib/X` -> `$MOUNT_ROOT/tmp/Y` the
    script names, then source /tmp/rt-kernel-plan.sh from that copy under setup.sh's `set -e` --
    no other repo lib is reachable from there."""
    root = tmp_path / "mnt"
    (root / "tmp").mkdir(parents=True)
    copies = re.findall(r'cp "\$SCRIPT_DIR/lib/([\w.-]+)" "\$MOUNT_ROOT/tmp/([\w.-]+)"',
                        _code(_read(CREATE_USB)))
    assert copies
    for src, dst in copies:
        shutil.copy(os.path.join(REPO, "scripts", "lib", src), root / "tmp" / dst)
    r = subprocess.run(["bash", "-c", 'set -e\nsource "$1/tmp/rt-kernel-plan.sh"\n'
                        'rt_kernel_flavour; echo; rt_kernel_line_held_metas',
                        "harness", str(root)], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    flavour, metas = r.stdout.split("\n", 1)
    assert flavour == FLAVOUR and metas.split() == HWE_METAS, r.stdout
