"""Issue 1387, 1.11.2026: the cameraman HDMI preview of a cambox is capped at 1920x1080 @ 60 Hz.

WHY: cam5's new operator monitor ("Field Monitor") prefers 3840x2160 @ 60 Hz (594 MHz) in its EDID.
i915's fbdev takes the EDID preferred mode, and that link could not carry it: the monitor read "HDMI no
signal" through the SNV production. The live fix was a runtime EDID override, which a reboot drops.
camera-box renders the preview at min(source, framebuffer) and never upscales, so a framebuffer above
1080p buys nothing. The persistent cap is the kernel's own `video=1920x1080@60` (the DRM fb helper
takes a global `video=` mode for every connector), written by setup-device STEP 10 into the same
idempotent GRUB_CMDLINE_LINUX loop as the core-isolation flags, and graded by verify-device (az):
the running kernel's cmdline carries it and fb0 is no larger than 1920x1080.

Tier-0: bash + pytest only.
"""
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SETUP = REPO / "scripts" / "setup-device.sh"
VERIFY = REPO / "scripts" / "verify-device.sh"
FLAG = "video=1920x1080@60"


def _grub_loop():
    lines = SETUP.read_text().splitlines()
    start = next(i for i, l in enumerate(lines) if l.startswith("for flag_tag in "))
    end = next(i for i in range(start, len(lines)) if lines[i] == "done")
    return "\n".join(lines[start:end + 1])


def test_setup_step10_appends_the_preview_cap_to_the_kernel_cmdline(tmp_path):
    grub = tmp_path / "grub"
    grub.write_text('GRUB_DEFAULT=saved\nGRUB_CMDLINE_LINUX="isolcpus=3"\n')
    loop = _grub_loop().replace("/etc/default/grub", str(grub))
    for _ in range(2):  # idempotent: the second run adds nothing
        r = subprocess.run(["bash", "-c", "set -euo pipefail\n" + loop], capture_output=True, text=True,
                           timeout=30)
        assert r.returncode == 0, r.stderr
    text = grub.read_text()
    line = next(l for l in text.splitlines() if l.startswith("GRUB_CMDLINE_LINUX="))
    assert line.split('"')[1].split().count(FLAG) == 1, line
    assert "isolcpus=3" in line


def _verdict(cmdline, fb):
    script = ('set -euo pipefail\n. "$1"\npreview_cap_verdict "$2" "$3"')
    r = subprocess.run(["bash", "-c", script, "harness", str(VERIFY), cmdline, fb], capture_output=True,
                       text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_the_verdict_needs_the_flag_as_a_whole_token():
    base = "BOOT_IMAGE=/vmlinuz root=UUID=x ro isolcpus=3"
    assert _verdict(base, "1920,1080") == "no-flag"
    assert _verdict(base + " video=1920x1080@600", "1920,1080") == "no-flag"
    assert _verdict(base + " xvideo=1920x1080@60", "1920,1080") == "no-flag"
    assert _verdict(base + " " + FLAG, "1920,1080") == "ok"
    assert _verdict(FLAG + " " + base, "1920,1080\n") == "ok"


def test_the_verdict_fails_a_framebuffer_above_1080p():
    cmd = "ro " + FLAG
    assert _verdict(cmd, "3840,2160") == "too-big:3840x2160"
    assert _verdict(cmd, "1920,1200") == "too-big:1920x1200"
    assert _verdict(cmd, "1280,720") == "ok"
    assert _verdict(cmd, "") == "ok", "no fbdev surface (no monitor at boot) is not a failure"
    assert _verdict(cmd, "x,y") == "unreadable:x,y"


def _run_az(ssh_out, ssh_rc=0):
    s = VERIFY.read_text()
    az = s.find("\n# (az) ")
    assert s.find("\n# (ak) ") < az < s.find("\n# (ay) ") < s.rfind("# (q) .bak cruft drift")
    block = s[az:s.find("\n# (", az + 10)]
    assert 'warn "' not in block
    # the stub ignores the remote text and answers what the box would print
    script = ('set -euo pipefail\n. "$1"\nok() { echo "PASS $1"; }\nfail() { echo "FAIL $1"; }\n'
              'SSH_OUT="$2"; SSH_RC="$3"\n'
              'ssh_box() { printf "%s" "$SSH_OUT"; return "$SSH_RC"; }\n'
              + block + '\necho END\n')
    r = subprocess.run(["bash", "-c", script, "harness", str(VERIFY), ssh_out, str(ssh_rc)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and r.stdout.strip().endswith("END"), r.stdout + r.stderr
    return r.stdout.strip().splitlines()[0]


def test_check_az_passes_a_capped_box_and_fails_every_other_state():
    good = "BOOT_IMAGE=/vmlinuz ro isolcpus=3 " + FLAG + "\n--fb--\n1920,1080\n"
    assert _run_az(good).startswith("PASS"), _run_az(good)
    assert "1920x1080" in _run_az(good)
    assert _run_az("BOOT_IMAGE=/vmlinuz ro isolcpus=3\n--fb--\n3840,2160\n").startswith("FAIL")
    assert _run_az("ro " + FLAG + "\n--fb--\n3840,2160\n").startswith("FAIL")
    assert _run_az("", 255).startswith("FAIL")


def test_check_az_is_documented_in_the_header_and_usage():
    s = VERIFY.read_text()
    assert s.count("(az) cameraman preview capped at 1080p") >= 3, "header, usage and the check itself"
