#!/usr/bin/env python3
"""Issue 1446 -- verify-device FAILs a cambox whose display GPU has no kernel driver bound.

Owner, 16.10.2026 (verbatim): "na cam5 pripojeny operator monitor neukazuje nic preco!!! ma vsetko
byt failproof a stale fungovat".

cam5/cam6/cam7 carry the N150 GPU 8086:46d4. The fleet kernel 6.8.0-142's i915 knows only
46D0-46D2, so that GPU had no driver; the cameraman HDMI preview then lived only on the firmware
framebuffer, which exists only when a monitor was on at power-on. Nothing in setup-device /
verify-device looked at the display controller, so those boxes passed verify with a dead output path.

The design (main, comment 6211296614, Approach 1): ONE pure lib, scripts/lib/display-gpu.sh.
`display_gpu_gather_remote_snippet` (run over ssh) prints a `kernel <uname -r>` row, one
`<slot> <vendor>:<device> <driver or ->` row per PCI display controller (class 0x03*) and a closing
`end` row; `display_gpu_verdict` (pure, on dev1) is OK only when every display controller has a
driver bound. No controller, a missing kernel row, a cut or malformed read all FAIL, never OK.
verify-device.sh grades it as the HARD item (av).

Tier-0: stdlib + bash only, no rig, no network. Values reach bash as ARGUMENTS or env, never inside
the script text (the issue-1302 harness rule).
"""
import os
import re
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "display-gpu.sh")
VERIFY_DEVICE = os.path.join(REPO, "scripts", "verify-device.sh")

# Real shapes, read live on 16.10.2026 (ticket body + comment 6211215555).
K68 = "6.8.0-142-generic"
K70 = "7.0.0-38-generic"
N150_NO_DRIVER = "0000:00:02.0 8086:46d4 -"      # cam5/6/7 on 6.8
N100_I915 = "0000:00:02.0 8086:46d1 i915"        # cam1/2/3/4
N150_I915 = "0000:00:02.0 8086:46d4 i915"        # cam5 after the HWE kernel


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


def _verdict(block):
    """(rc, stdout) of display_gpu_verdict over BLOCK, under the caller's strict mode."""
    r = _lib('rc=0; display_gpu_verdict "$1" || rc=$?; echo "RC=$rc"', block)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "unbound variable" not in r.stderr, r.stderr
    out, _, rc = r.stdout.rpartition("RC=")
    return int(rc.strip()), out


def _rows(*rows, kernel=K68, end=True):
    lines = [f"kernel {kernel}"] if kernel is not None else []
    lines += list(rows)
    if end:
        lines.append("end")
    return "\n".join(lines) + "\n"


# --- the verdict over real row shapes ----------------------------------------------------------

def test_n150_with_no_driver_fails_naming_the_id_the_kernel_and_the_remedy():
    rc, out = _verdict(_rows(N150_NO_DRIVER))
    assert rc != 0, out
    assert out.startswith("FAIL: "), out
    assert "8086:46d4" in out and "0000:00:02.0" in out, out
    assert K68 in out, out
    assert "scripts/rt-kernel-upgrade.sh" in out and ".claude/rules/realtime-isolation.md" in out, out
    assert "HWE" in out, out
    assert not out.startswith("OK"), out


def test_n100_with_i915_bound_is_ok_and_names_what_it_graded():
    rc, out = _verdict(_rows(N100_I915))
    assert rc == 0, out
    assert out.startswith("OK: "), out
    assert "FAIL" not in out
    assert "8086:46d1" in out and "i915" in out and K68 in out, out


def test_n150_on_the_hwe_kernel_with_i915_bound_is_ok():
    rc, out = _verdict(_rows(N150_I915, kernel=K70))
    assert rc == 0 and out.startswith("OK: ") and K70 in out, out


def test_no_display_controller_at_all_fails():
    rc, out = _verdict(_rows())
    assert rc != 0 and out.startswith("FAIL: "), out
    assert "no PCI display controller" in out, out
    assert K68 in out, out


def test_an_empty_or_unreadable_gather_fails_never_ok():
    for block in ("", "\n", "   \n"):
        rc, out = _verdict(block)
        assert rc != 0 and out.startswith("FAIL: "), (block, out)
        assert "OK" not in out, out


def test_a_cut_read_without_the_end_row_fails_even_with_a_driven_gpu():
    rc, out = _verdict(_rows(N100_I915, end=False))
    assert rc != 0 and out.startswith("FAIL: "), out
    assert "cut" in out or "could not be read" in out, out


def test_a_missing_kernel_row_fails():
    rc, out = _verdict(_rows(N100_I915, kernel=None))
    assert rc != 0 and out.startswith("FAIL: "), out
    assert "kernel" in out, out


def test_an_empty_kernel_value_fails():
    rc, out = _verdict("kernel \n" + N100_I915 + "\nend\n")
    assert rc != 0 and out.startswith("FAIL: "), out


def test_a_malformed_controller_row_fails_by_name():
    for bad in ("0000:00:02.0 8086-46d1 i915",      # id not vendor:device
                "0000:00:02.0 8086:46d1",            # no driver column
                "0000:00:02.0 8086:46d1 i915 extra",  # trailing junk
                "0000:00:02.0 8086:46dz i915",       # not hex
                "0000:00:02.0 0x8086:0x46d1 i915"):  # not stripped
        rc, out = _verdict(_rows(N100_I915, bad))
        assert rc != 0 and out.startswith("FAIL: "), (bad, out)
        assert "malformed" in out, (bad, out)


def test_every_undriven_controller_gets_its_own_fail_line_and_a_driven_one_does_not_hide_it():
    rc, out = _verdict(_rows("0000:01:00.0 10de:28e0 nvidia", N150_NO_DRIVER,
                             "0000:00:03.0 8086:46d3 -"))
    assert rc != 0, out
    fails = [ln for ln in out.splitlines() if ln.startswith("FAIL: ")]
    assert len(fails) == 2, out
    assert "8086:46d4" in fails[0] and "8086:46d3" in fails[1], out
    assert "10de:28e0" not in out, out


def test_two_driven_controllers_are_ok():
    rc, out = _verdict(_rows("0000:01:00.0 10de:28e0 nvidia", N100_I915))
    assert rc == 0 and out.startswith("OK: ") and "10de:28e0" in out and "8086:46d1" in out, out


def test_the_verdict_is_ascii_only_under_a_utf8_locale():
    # a non-ASCII digit in the id must not pass the hex check (the issue-1302 locale trap)
    env = {"LC_ALL": "en_US.UTF-8"}
    r = _lib('rc=0; display_gpu_verdict "$1" || rc=$?; echo "RC=$rc"',
             _rows("0000:00:02.0 8086:46d١ i915"), env=env)
    assert r.returncode == 0, r.stderr
    assert "RC=0" not in r.stdout and "FAIL: " in r.stdout, r.stdout


# --- the gather snippet, run locally ------------------------------------------------------------

def _fake_pci(tmp_path, devices):
    """A fake /sys/bus/pci/devices: DEVICES = [(slot, class, vendor, device, driver or None)]."""
    root = tmp_path / "pci"
    root.mkdir()
    for slot, cls, ven, dev, drv in devices:
        d = root / slot
        d.mkdir()
        (d / "class").write_text(cls + "\n")
        (d / "vendor").write_text(ven + "\n")
        (d / "device").write_text(dev + "\n")
        if drv:
            os.symlink(f"../../../bus/pci/drivers/{drv}", d / "driver")
    return root


def _gather(env=None):
    r = _lib('snip="$(display_gpu_gather_remote_snippet)"\nbash -c "set -euo pipefail\n$snip"', env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    return r.stdout


def test_the_gather_on_a_fake_cam5_tree_feeds_the_verdict_a_fail(tmp_path):
    root = _fake_pci(tmp_path, [
        ("0000:00:00.0", "0x060000", "0x8086", "0x461c", "igen6_edac"),   # host bridge, not display
        ("0000:00:02.0", "0x030000", "0x8086", "0x46d4", None),           # the N150 GPU, no driver
        ("0000:00:14.0", "0x0c0330", "0x8086", "0x54ed", "xhci_hcd"),     # USB, not display
    ])
    out = _gather(env={"DISPLAY_GPU_PCI_ROOT": str(root)})
    lines = out.splitlines()
    assert lines[0] == f"kernel {os.uname().release}", out
    assert lines[1:] == ["0000:00:02.0 8086:46d4 -", "end"], out
    rc, v = _verdict(out)
    assert rc != 0 and "8086:46d4" in v, v


def test_the_gather_on_a_fake_driven_tree_feeds_the_verdict_ok(tmp_path):
    root = _fake_pci(tmp_path, [
        ("0000:00:02.0", "0x030000", "0x8086", "0x46d1", "i915"),
        ("0000:01:00.0", "0x030200", "0x10de", "0x28e0", "nvidia"),       # 3D controller, class 0x0302
    ])
    out = _gather(env={"DISPLAY_GPU_PCI_ROOT": str(root)})
    assert "0000:00:02.0 8086:46d1 i915" in out and "0000:01:00.0 10de:28e0 nvidia" in out, out
    rc, v = _verdict(out)
    assert rc == 0 and v.startswith("OK: "), v


def test_the_gather_on_an_empty_tree_reports_no_controller(tmp_path):
    root = tmp_path / "pci"
    root.mkdir()
    out = _gather(env={"DISPLAY_GPU_PCI_ROOT": str(root)})
    assert out.splitlines()[-1] == "end" and len(out.splitlines()) == 2, out
    rc, v = _verdict(out)
    assert rc != 0 and "no PCI display controller" in v, v


def test_the_gather_on_this_machines_real_sysfs_parses(tmp_path):
    """The default root (/sys/bus/pci/devices): every row the real tree yields must parse.
    A runner may have no display controller; that is a named FAIL, never a malformed one."""
    out = _gather()
    lines = out.splitlines()
    assert lines[0] == f"kernel {os.uname().release}" and lines[-1] == "end", out
    for ln in lines[1:-1]:
        assert re.fullmatch(r"[0-9a-f:.]+ [0-9a-f]{4}:[0-9a-f]{4} \S+", ln), ln
    rc, v = _verdict(out)
    assert "malformed" not in v and "could not be read" not in v, v
    assert v.startswith("OK: ") or "no PCI display controller" in v or "no kernel driver" in v, v


def test_the_lib_is_source_only_and_strict_mode_safe():
    text = _read(LIB)
    assert "set -euo pipefail" not in _code(text), "a sourced lib must not set strict mode for the caller"
    # sourcing has no side effect: no output, no error
    r = _lib("true")
    assert r.returncode == 0 and r.stdout == "" and r.stderr == "", r


# --- verify-device (av) ------------------------------------------------------------------------

def _av_block(text):
    s = text.find("\n# (av) ")
    e = text.find("\n# (au) ", s)
    assert 0 <= s < e, "(av) sits right before (au)"
    return text[s:e]


def test_verify_device_documents_sources_and_places_av():
    text = _read(VERIFY_DEVICE)
    assert text.count("(av) display GPU driven (issue 1446)") == 3, "header, usage() and the check"
    header = text[:text.find("set -euo pipefail")]
    assert "(av) display GPU driven (issue 1446)" in header
    usage = text[text.find("usage() {"):]
    assert "(av) display GPU driven (issue 1446)" in usage[:usage.find("\nEOF\n")]
    assert re.search(r'(?m)^\. "\$HERE/lib/display-gpu\.sh"', text)
    guard = text.find("never run the live SSH flow below.")
    av = text.find("\n# (av) ")
    q = text.rfind("# (q) .bak cruft drift")
    # after (ak), before (au): outside every slice another test EXECUTES
    # ((as)..(ar), (ar)..(al), (aq)..(ao), (ao)..(an), (an)..(q)), and (q) stays last
    assert guard < text.find("\n# (ak) ") < av < text.find("\n# (au) ") < text.find("\n# (at) ") < q
    assert not (text.find("\n# (as) ") < av < q)


def test_verify_device_av_is_a_hard_gate_over_the_shared_lib():
    code = _code(_av_block(_read(VERIFY_DEVICE)))
    assert "display_gpu_gather_remote_snippet" in code and "display_gpu_verdict" in code
    assert 'fail "' in code and 'warn "' not in code


def _run_av(ssh_out, ssh_rc=0):
    block = _av_block(_read(VERIFY_DEVICE))
    prelude = (f'. "{LIB}"\n'
               'ok() { echo "OK $1"; }\nfail() { echo "FAIL $1"; }\n'
               'ssh_box() { printf "%s" "$SSH_OUT"; return "$SSH_RC"; }\n')
    r = _bash("set -euo pipefail\n" + prelude + block,
              env={"SSH_OUT": ssh_out, "SSH_RC": str(ssh_rc)})
    assert r.returncode == 0, r.stdout + r.stderr
    return r.stdout


def test_verify_device_av_passes_a_driven_gpu_and_fails_n150_on_68_and_unreadable():
    out = _run_av(_rows(N100_I915))
    assert out.startswith("OK ") and "FAIL" not in out and "8086:46d1" in out, out
    out = _run_av(_rows(N150_NO_DRIVER))
    assert out.startswith("FAIL ") and "8086:46d4" in out and K68 in out, out
    assert "OK " not in out, out
    out = _run_av("")
    assert out.startswith("FAIL ") and "OK " not in out, out
    out = _run_av("", ssh_rc=255)
    assert out.startswith("FAIL ") and "rc=255" in out, out
    out = _run_av(_rows())
    assert out.startswith("FAIL ") and "no PCI display controller" in out, out
