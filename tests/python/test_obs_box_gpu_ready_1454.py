"""issue 1454: lightdm starts X only once every display GPU is registered with udev.

strih-snv booted on 21.10.2026 to the lightdm LOGIN SCREEN instead of the autologin kiosk. lightdm's
first X server started 0.5 s after the amdgpu iGPU finished its DRM init, before udev had finished that
card; X added it later as a hotplugged GPU screen and Xorg aborted in glamor_init
(`dixRegisterPrivateKey: Assertion '!global_keys[type].created' failed`, core dump of Xorg). lightdm then
started the greeter. Owner (verbatim): "nesmie tam byt ziadny prihlasovanie ma sa hned vsetko spustit a
bezat!!!! to iste na vsetkych nasich linux rigoch".

The fix lives in the shared kiosk lib (every Linux OBS box): a lightdm.service drop-in runs a bounded
wait (`obs-box-gpu-ready-wait 30`) until every PCI display-class device has a DRM card udev finished
(USEC_INITIALIZED); past the bound it warns and lets lightdm start anyway. The shared grader's `kiosk`
row FAILs while the wait is not installed and on a boot whose Xorg dumped core.
"""

import os
import stat
import subprocess
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
BASELINE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
VERIFY_LIB = REPO / "scripts" / "lib" / "obs-box-baseline-verify.sh"
WAIT_PATH = "/usr/local/sbin/obs-box-gpu-ready-wait"
DROPIN_PATH = "/etc/systemd/system/lightdm.service.d/50-obs-box-gpu-ready.conf"
ME = "%s:%s" % (subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip(),
                subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip())


def _bash(lib, body, args=(), env=None, timeout=60):
    """Run BODY under the callers' strict mode with LIB sourced and a caller-style fail()."""
    script = 'set -euo pipefail\nfail() { echo "FAIL: $1" >&2; exit 1; }\n. "$1"\nshift\n' + body
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "harness", str(lib), *args], capture_output=True,
                          text=True, timeout=timeout, env=e)


def _render(fn):
    r = _bash(BASELINE, fn)
    assert r.returncode == 0, r.stderr
    return r.stdout


# ---------------------------------------------------------------------------------------------
# the waiter, run for real against a fake sysfs tree and a fake udevadm
# ---------------------------------------------------------------------------------------------


def _waiter(tmp_path):
    w = tmp_path / "obs-box-gpu-ready-wait"
    w.write_text(_render("obs_box_gpu_ready_wait_text"))
    w.chmod(0o755)
    return w


def _pci(root, name, cls, card=None):
    d = root / "bus" / "pci" / "devices" / name
    d.mkdir(parents=True)
    (d / "class").write_text(cls + "\n")
    if card:
        (d / "drm" / card).mkdir(parents=True)
    return d


def _udevadm(tmp_path):
    """A fake udevadm: `info ... --name=/dev/dri/cardN` prints USEC_INITIALIZED only when the file
    <tmp>/udev-done/cardN exists; `settle` is logged. Every call goes to <tmp>/udevadm.log."""
    done = tmp_path / "udev-done"
    done.mkdir()
    log = tmp_path / "udevadm.log"
    f = tmp_path / "udevadm"
    f.write_text(
        "#!/bin/bash\n"
        f'echo "$*" >> "{log}"\n'
        'if [ "$1" = info ]; then\n'
        '  for a in "$@"; do case "$a" in --name=*) card="${a##*/}" ;; esac; done\n'
        '  echo "DEVNAME=/dev/dri/${card}"\n'
        f'  [ -e "{done}/${{card}}" ] && echo "USEC_INITIALIZED=12345"\n'
        "fi\n"
        "exit 0\n")
    f.chmod(0o755)
    return f, done, log


def _run_waiter(tmp_path, root, udevadm, budget="5"):
    t0 = time.monotonic()
    r = subprocess.run([str(_waiter(tmp_path)), budget], capture_output=True, text=True, timeout=60,
                       env=dict(os.environ, OBS_BOX_GPU_READY_SYSFS=str(root),
                                OBS_BOX_GPU_READY_UDEVADM=str(udevadm)))
    return r, time.monotonic() - t0


def test_ready_gpus_pass_at_once_and_settle_udev(tmp_path):
    root = tmp_path / "sys"
    _pci(root, "0000:01:00.0", "0x030000", "card1")    # NVIDIA
    _pci(root, "0000:64:00.0", "0x030000", "card2")    # AMD iGPU
    fake, done, log = _udevadm(tmp_path)
    (done / "card1").touch()
    (done / "card2").touch()
    r, took = _run_waiter(tmp_path, root, fake)
    assert r.returncode == 0, r.stderr
    assert "every display GPU is registered with udev" in r.stdout
    assert "WARNING" not in r.stdout
    assert took < 3
    assert "settle" in log.read_text()


def test_a_late_card_is_waited_for(tmp_path):
    """The strih-snv race: the iGPU's DRM card and its udev record come after the waiter started."""
    root = tmp_path / "sys"
    _pci(root, "0000:01:00.0", "0x030000", "card1")
    igpu = _pci(root, "0000:64:00.0", "0x030000")
    fake, done, _ = _udevadm(tmp_path)
    (done / "card1").touch()

    def late():
        time.sleep(0.8)
        (igpu / "drm" / "card2").mkdir(parents=True)
        time.sleep(0.6)
        (done / "card2").touch()

    t = threading.Thread(target=late)
    t.start()
    r, took = _run_waiter(tmp_path, root, fake)
    t.join()
    assert r.returncode == 0, r.stderr
    assert "every display GPU is registered with udev" in r.stdout
    assert "WARNING" not in r.stdout
    assert 1.2 <= took < 5


def test_a_card_udev_has_not_finished_is_still_pending(tmp_path):
    """The card directory alone is not enough: X enumerates GPUs from the udev database."""
    root = tmp_path / "sys"
    _pci(root, "0000:64:00.0", "0x030000", "card2")
    fake, _, _ = _udevadm(tmp_path)
    r, took = _run_waiter(tmp_path, root, fake, budget="1")
    assert r.returncode == 0
    assert "WARNING obs-box-gpu-ready" in r.stdout
    assert "0000:64:00.0 (card2 not finished by udev yet)" in r.stdout
    assert 1 <= took < 4


def test_a_gpu_that_never_comes_is_bounded_and_never_fatal(tmp_path):
    root = tmp_path / "sys"
    _pci(root, "0000:64:00.0", "0x030000")
    fake, _, _ = _udevadm(tmp_path)
    r, took = _run_waiter(tmp_path, root, fake, budget="1")
    assert r.returncode == 0, "the display manager must still start"
    assert "still waiting after 1 s for: 0000:64:00.0 (no DRM card yet)" in r.stdout
    assert "starting the display manager anyway" in r.stdout
    assert took < 4


def test_non_display_devices_are_ignored(tmp_path):
    """The NPU (0x118000), a NIC (0x020000) and a bridge carry no DRM card and must not hold X."""
    root = tmp_path / "sys"
    _pci(root, "0000:65:00.1", "0x118000")
    _pci(root, "0000:02:00.0", "0x020000")
    _pci(root, "0000:00:00.0", "0x060000")
    _pci(root, "0000:01:00.0", "0x030200", "card1")    # a 3D controller is display class too
    fake, done, _ = _udevadm(tmp_path)
    (done / "card1").touch()
    r, took = _run_waiter(tmp_path, root, fake, budget="3")
    assert r.returncode == 0
    assert "every display GPU is registered with udev" in r.stdout
    assert took < 2


def test_a_bad_budget_falls_back_to_30_s(tmp_path):
    root = tmp_path / "sys"
    root.mkdir()
    fake, _, _ = _udevadm(tmp_path)
    r, _ = _run_waiter(tmp_path, root, fake, budget="1x")
    assert r.returncode == 0 and "registered with udev" in r.stdout
    text = _render("obs_box_gpu_ready_wait_text")
    assert 'case "$budget" in \'\' | *[!0123456789]*) budget=30 ;; esac' in text


# ---------------------------------------------------------------------------------------------
# the install + the drop-in
# ---------------------------------------------------------------------------------------------


def test_the_drop_in_runs_the_waiter_before_lightdm_starts_x():
    text = _render("obs_box_gpu_ready_dropin_text")
    assert "[Service]\n" in text
    assert f"ExecStartPre={WAIT_PATH} 30\n" in text
    assert _render("obs_box_gpu_ready_wait_path") == WAIT_PATH
    assert _render("obs_box_gpu_ready_dropin_path") == DROPIN_PATH


def test_install_writes_both_files_and_is_idempotent(tmp_path):
    w = tmp_path / "sbin" / "obs-box-gpu-ready-wait"
    d = tmp_path / "lightdm.service.d" / "50-obs-box-gpu-ready.conf"
    body = 'obs_box_gpu_ready_install "$1" "$2" "$3"'
    r = _bash(BASELINE, body, args=(str(w), str(d), ME))
    assert r.returncode == 0, r.stderr
    assert w.read_text() == _render("obs_box_gpu_ready_wait_text")
    assert d.read_text() == _render("obs_box_gpu_ready_dropin_text")
    assert stat.S_IMODE(w.stat().st_mode) == 0o755
    assert stat.S_IMODE(d.stat().st_mode) == 0o644
    assert "written" in r.stdout
    again = _bash(BASELINE, body, args=(str(w), str(d), ME))
    assert again.returncode == 0, again.stderr
    assert "written" not in again.stdout and again.stdout.count("unchanged") == 2


def test_the_kiosk_item_installs_the_wait():
    r = subprocess.run(["bash", "-c", '. "$1"; declare -f obs_box_kiosk', "h", str(BASELINE)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    assert "obs_box_gpu_ready_install" in r.stdout


# ---------------------------------------------------------------------------------------------
# the grader: the real gather (its two paths moved to fixtures) and the kiosk row
# ---------------------------------------------------------------------------------------------


def _gather_facts(tmp_path, journal_text):
    snippet = subprocess.run(
        ["bash", "-c", '. "$1"; obs_box_baseline_gather_snippet strih newlevel strih-obs.service', "h",
         str(VERIFY_LIB)], capture_output=True, text=True, timeout=30)
    assert snippet.returncode == 0, snippet.stderr
    text = snippet.stdout
    assert text.count(WAIT_PATH) == 1 and text.count(DROPIN_PATH) == 1, \
        "the gather must read the paths through the ONE embedded path function each, never retype them"
    w = tmp_path / "wait"
    d = tmp_path / "dropin"
    jfile = tmp_path / "journal.txt"
    jfile.write_text(journal_text)
    stub = (f'obs_box_gpu_ready_wait_path() {{ printf %s "{w}"; }}\n'
            f'obs_box_gpu_ready_dropin_path() {{ printf %s "{d}"; }}\n'
            f'journalctl() {{ cat "{jfile}"; }}\n')
    # the embedded path functions come first in the snippet; the stubs redefine them after
    text = text.replace("set +e\n", "set +e\n" + stub, 1)
    return w, d, text


def _run_gather(text):
    r = subprocess.run(["bash", "-c", text], capture_output=True, text=True, timeout=120)
    facts = {}
    for line in r.stdout.splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            facts[k] = v
    return facts


def test_the_gather_reads_the_installed_wait_and_the_xorg_core_dumps(tmp_path):
    w, d, text = _gather_facts(tmp_path, "Process 2461 (Xorg) of user 0 terminated abnormally with signal 6/ABRT, processing...\n")
    facts = _run_gather(text)
    assert facts["gpu_wait"] == "0"
    assert facts["x_core"] == "1"
    w.write_text(_render("obs_box_gpu_ready_wait_text"))
    w.chmod(0o755)
    d.write_text(_render("obs_box_gpu_ready_dropin_text"))
    facts = _run_gather(text)
    assert facts["gpu_wait"] == "1"
    w.write_text(_render("obs_box_gpu_ready_wait_text") + "# edited by hand\n")
    assert _run_gather(text)["gpu_wait"] == "0"


def test_a_clean_boot_reads_no_xorg_core(tmp_path):
    _, _, text = _gather_facts(tmp_path, "Process 491103 (companion-satellite) of user 1000 dumped core.\n")
    assert _run_gather(text)["x_core"] == "0"


def _kiosk_row(gpu_wait, x_core):
    facts = "\n".join([
        "gather_done=1", "lightdm=install ok installed", "openbox=install ok installed",
        "dm=/usr/lib/systemd/system/lightdm.service", "dm_lightdm=/usr/lib/systemd/system/lightdm.service",
        "autologin=1", "gdm3=", "gnome_shell=", f"gpu_wait={gpu_wait}", f"x_core={x_core}"]) + "\n"
    r = subprocess.run(["bash", "-c", '. "$1"; obs_box_baseline_verdict', "h", str(VERIFY_LIB)],
                       input=facts, capture_output=True, text=True, timeout=30)
    rows = [line for line in r.stdout.splitlines() if line.startswith("kiosk|")]
    assert len(rows) == 1, r.stdout
    return rows[0]


def test_the_kiosk_row_needs_the_gpu_wait_and_a_boot_without_an_xorg_core():
    assert _kiosk_row("1", "0").startswith("kiosk|OK|")
    missing = _kiosk_row("0", "0")
    assert missing.startswith("kiosk|FAIL|") and "gpu-ready wait installed=0" in missing
    crashed = _kiosk_row("1", "1")
    assert crashed.startswith("kiosk|FAIL|") and "Xorg core dumps this boot=1" in crashed
    assert _kiosk_row("1", "").startswith("kiosk|FAIL|"), "an unread count is never a pass"
