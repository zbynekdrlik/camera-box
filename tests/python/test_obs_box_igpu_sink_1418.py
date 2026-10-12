"""issue 1418: the iGPU panel sink of an NVIDIA-primary box runs on the Xorg `modesetting` DDX.

strih-snv 11/12.10.2026: NVIDIA renders the X screen (PRIME nvidia-primary) and the AMD Radeon 780M
iGPU scans the panel out as a reverse-PRIME sink. Xorg loaded xf86-video-amdgpu for that sink, and its
panel commit (`commit_work`) blocked forever on an NVIDIA fence: the panel froze for 18 h. The Intel
notebook before it had the same topology with the sink on `modesetting` and was stable
(ROZHODNUTE 6132283178).

The live fix on strih-snv (12.10.2026 23:00): an /etc/X11/xorg.conf.d OutputClass `MatchDriver
"amdgpu"` -> `Driver "modesetting"` does NOT win (tried as 20-, 05- and a same-named 10-amdgpu.conf
shadow): Xorg applies the package's own /usr/share/X11/xorg.conf.d/10-amdgpu.conf first. Removing
`xserver-xorg-video-amdgpu` (+ the `xserver-xorg-video-all` meta, its only reverse dependency) does:
the sink reads `name:modesetting`. So the shared baseline's NVIDIA/PRIME step removes the two packages
on a discrete-NVIDIA box (and still writes the OutputClass file, belt and braces), and the ONE baseline
grader FAILs its `igpu-sink` row while the amdgpu DDX is installed on such a box.
"""

import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
BASELINE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
VERIFY_LIB = REPO / "scripts" / "lib" / "obs-box-baseline-verify.sh"
KIOSK = REPO / "scripts" / "lib" / "obs-box-kiosk.sh"
SETUPS = [REPO / "scripts" / "setup-strih.sh", REPO / "scripts" / "setup-imag.sh"]
OPERATOR_LIB = REPO / "scripts" / "lib" / "strih-operator.sh"
CONF = "/etc/X11/xorg.conf.d/20-igpu-sink-modesetting.conf"
DDX = "xserver-xorg-video-amdgpu"
META = "xserver-xorg-video-all"
SECTION = ['Section "OutputClass"', '    Identifier "igpu-sink-modesetting"', '    MatchDriver "amdgpu"',
           '    Driver "modesetting"', "EndSection"]
ME = "%s:%s" % (subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip(),
                subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip())

# dpkg-query / apt-get stand-ins as bash FUNCTIONS (they shadow the real tools, so a test never removes
# a package): $INSTALLED lists the installed packages (dpkg-query's real "no packages found" otherwise),
# $DPKG_BROKEN=1 makes dpkg-query fail like a broken status file, $EXTRA_REMV what a simulated remove
# would take besides them, $APT_FAIL=1 fails the real remove. Every call is logged to $LOG.
FAKE_APT = r'''
dpkg-query() {
    echo "dpkg-query $*" >> "$LOG"
    if [ "${DPKG_BROKEN:-0}" = 1 ]; then echo "dpkg-query: error: parsing file '/var/lib/dpkg/status'" >&2; return 2; fi
    case " $INSTALLED " in
        *" ${@: -1} "*) printf 'install ok installed' ;;
        *) echo "dpkg-query: no packages found matching ${@: -1}" >&2; return 1 ;;
    esac
}
apt-get() {
    echo "apt-get $*" >> "$LOG"
    local a
    if [ "$1" = -s ]; then
        for a in "${@:3}" $EXTRA_REMV; do echo "Remv $a [1.0]"; done
        return 0
    fi
    [ "${APT_FAIL:-0}" != 1 ] || { echo "E: dpkg was interrupted" >&2; return 100; }
}
'''


def _bash(lib, body, args=(), env=None):
    """Run BODY under the callers' strict mode with LIB sourced and a caller-style fail()."""
    script = 'set -euo pipefail\nfail() { echo "FAIL: $1" >&2; exit 1; }\n. "$1"\nshift\n' + body
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "harness", str(lib), *args], capture_output=True,
                          text=True, timeout=120, env=e)


def _conf_text():
    r = _bash(BASELINE, "obs_box_igpu_sink_modesetting_conf")
    assert r.returncode == 0, r.stderr
    return r.stdout


def _remove(tmp_path, installed, extra="", apt_fail="0", broken="0"):
    log = tmp_path / "calls"
    log.write_text("")
    r = _bash(BASELINE, FAKE_APT + "obs_box_amdgpu_ddx_remove",
              env={"LOG": str(log), "INSTALLED": installed, "EXTRA_REMV": extra, "APT_FAIL": apt_fail,
                   "DPKG_BROKEN": broken})
    return r, log.read_text().splitlines()


def test_the_amdgpu_ddx_packages_are_the_ddx_and_its_meta():
    assert _bash(BASELINE, "obs_box_amdgpu_ddx_package").stdout == DDX
    assert _bash(BASELINE, "obs_box_amdgpu_ddx_packages").stdout.split() == [DDX, META]


def test_nothing_installed_removes_nothing(tmp_path):
    r, calls = _remove(tmp_path, "")
    assert r.returncode == 0, r.stderr
    assert "unchanged" in r.stdout and not [c for c in calls if c.startswith("apt-get")], calls


def test_the_installed_ddx_and_meta_are_removed_after_a_clean_simulation(tmp_path):
    r, calls = _remove(tmp_path, "%s %s" % (DDX, META))
    assert r.returncode == 0, r.stderr
    sim = "apt-get -s remove %s %s" % (DDX, META)
    real = "apt-get remove -y %s %s" % (DDX, META)
    assert sim in calls and real in calls and calls.index(sim) < calls.index(real), calls
    assert "removed" in r.stdout and DDX in r.stdout


def test_only_the_installed_ones_are_named(tmp_path):
    r, calls = _remove(tmp_path, DDX)
    assert r.returncode == 0, r.stderr
    assert "apt-get remove -y %s" % DDX in calls, calls


def test_a_removal_that_would_take_more_packages_is_refused(tmp_path):
    """Never remove xserver-xorg (and the session with it) to get rid of the DDX."""
    r, calls = _remove(tmp_path, "%s %s" % (DDX, META), extra="xserver-xorg lightdm")
    assert r.returncode != 0 and "xserver-xorg lightdm" in r.stderr, r.stdout + r.stderr
    assert not [c for c in calls if c.startswith("apt-get remove")], calls


def test_a_failed_removal_fails_loud(tmp_path):
    r, _calls = _remove(tmp_path, DDX, apt_fail="1")
    assert r.returncode != 0 and "FAIL" in r.stderr and DDX in r.stderr, r.stderr


def test_a_dpkg_that_cannot_answer_fails_loud_never_reads_as_not_installed(tmp_path):
    r, calls = _remove(tmp_path, DDX, broken="1")
    assert r.returncode != 0 and "FAIL" in r.stderr and DDX in r.stderr, r.stdout + r.stderr
    assert not [c for c in calls if c.startswith("apt-get")], calls


@pytest.mark.parametrize("status,present", [
    ("install ok installed", True), ("install ok half-installed", True), ("install ok unpacked", True),
    ("absent", False), ("unknown ok not-installed", False), ("deinstall ok config-files", False),
])
def test_the_status_classifier_counts_every_unpacked_state_as_present(status, present):
    r = _bash(BASELINE, 'obs_box_pkg_status_absent "$1"', (status,))
    assert (r.returncode != 0) == present, (status, r.returncode)


def test_the_kiosk_removes_the_ddx_again_after_its_xorg_install():
    """A fresh 26.04 box gets xserver-xorg from the kiosk; its `video-all | xorg-driver-video` dependency
    pulls the amdgpu DDX back in AFTER the NVIDIA step removed it, so the kiosk removes it again."""
    k = KIOSK.read_text()
    body = k[k.index("\nobs_box_kiosk() {"):]
    body = body[:body.index("\n}\n")]
    install = body.index("apt-get install -y openbox lightdm")
    assert "xserver-xorg" in body[install:body.index("\n", install)]
    call = body.index("    obs_box_amdgpu_ddx_remove")
    gate = body.rindex("if lspci -nn 2>/dev/null | obs_box_has_discrete_nvidia; then", 0, call)
    assert install < gate < call < body.index("\nfi\n", gate)
    for setup in SETUPS:
        t = setup.read_text()
        assert t.index("obs_box_nvidia_prime ") < t.index("obs_box_kiosk "), setup


def test_the_conf_maps_an_amdgpu_sink_onto_modesetting():
    lines = _conf_text().splitlines()
    i = lines.index(SECTION[0])
    assert lines[i:i + len(SECTION)] == SECTION
    assert all(ln.startswith("#") for ln in lines[:i]), "only comment lines before the section"
    assert "10-amdgpu.conf" in _conf_text(), "the file says it is belt and braces, never the fix"
    assert _bash(BASELINE, "obs_box_igpu_sink_conf_path").stdout == CONF


def test_the_install_removes_the_ddx_and_writes_the_conf_once(tmp_path):
    dest = tmp_path / "xorg.conf.d" / "20-igpu-sink-modesetting.conf"
    log = tmp_path / "calls"
    env = {"LOG": str(log), "INSTALLED": DDX, "EXTRA_REMV": "", "APT_FAIL": "0"}
    r = _bash(BASELINE, FAKE_APT + 'obs_box_igpu_sink_modesetting "$1" "$2"', (str(dest), ME), env)
    assert r.returncode == 0, r.stderr
    assert "apt-get remove -y %s" % DDX in log.read_text()
    assert "written" in r.stdout and dest.read_text() == _conf_text()
    assert oct(dest.stat().st_mode & 0o777) == "0o644"
    env["INSTALLED"] = ""
    r = _bash(BASELINE, FAKE_APT + 'obs_box_igpu_sink_modesetting "$1" "$2"', (str(dest), ME), env)
    assert r.returncode == 0 and r.stdout.count("unchanged") == 2, r.stdout + r.stderr


def test_the_nvidia_prime_step_installs_it_on_a_discrete_nvidia_box(tmp_path):
    """The real obs_box_nvidia_prime with its host tools stubbed as functions, on a dGPU box."""
    body = r'''
lspci() { echo '01:00.0 VGA compatible controller [0300]: NVIDIA Corporation AD107M [10de:28e0]'; }
dpkg() { echo 'Status: install ok installed'; }
safe_grub_regen() { :; }
nvidia-smi() { echo 'GPU 0: NVIDIA'; }
LOG="$1"
prime-select() { echo "prime-select $*" >> "$LOG"; }
obs_box_igpu_sink_modesetting() { echo "igpu-sink ${*:-<defaults>}" >> "$LOG"; }
obs_box_nvidia_prime strih
'''
    log = tmp_path / "calls"
    r = _bash(BASELINE, body, (str(log),))
    assert r.returncode == 0, r.stderr
    assert log.read_text().splitlines() == ["prime-select nvidia", "igpu-sink <defaults>"]


def test_the_iGPU_only_branch_never_runs_it():
    lib = BASELINE.read_text()
    body = lib[lib.index("\nobs_box_nvidia_prime() {"):]
    body = body[:body.index("\n}\n")]
    call = body.index("    obs_box_igpu_sink_modesetting")
    gate = body.rindex("if lspci -nn | obs_box_has_discrete_nvidia; then", 0, call)
    assert body.index("prime-select nvidia ||", gate) < call < body.index("\nfi\n", gate)
    assert body.count("obs_box_igpu_sink_modesetting") == 1


def _gather(dpkg_answer):
    """The real gather snippet, dpkg-query shadowed by a function; returns its amdgpu_ddx value.
    dpkg_answer: a status text, None = dpkg knows no such package, "BROKEN" = dpkg-query errors."""
    snip = _bash(VERIFY_LIB, 'obs_box_baseline_gather_snippet strih "$1" strih-obs.service',
                 (os.environ.get("USER") or "nobody",))
    assert snip.returncode == 0, snip.stderr
    if dpkg_answer == "BROKEN":
        answer = "echo 'dpkg-query: error: parsing file' >&2; return 2"
    elif dpkg_answer is None:
        answer = "echo \"dpkg-query: no packages found matching $p\" >&2; return 1"
    else:
        answer = "printf %%s '%s'; return 0" % dpkg_answer
    fake = ('dpkg-query() { local p="${@: -1}"; if [ "$p" = %s ]; then %s; fi; '
            'echo "dpkg-query: no packages found matching $p" >&2; return 1; }\n' % (DDX, answer))
    out = subprocess.run(["bash", "-c", fake + snip.stdout], capture_output=True, text=True,
                         timeout=120).stdout
    vals = [ln.split("=", 1)[1] for ln in out.splitlines() if ln.startswith("amdgpu_ddx=")]
    assert len(vals) == 1, out
    return vals[0]


def test_the_gather_reads_the_ddx_package_status():
    assert _gather("install ok installed") == "install ok installed"
    assert _gather("deinstall ok config-files") == "deinstall ok config-files"
    assert _gather(None) == "absent"
    assert _gather("BROKEN") == "", "a dpkg-query that errors is unread, never 'absent'"


def _rows(facts):
    r = _bash(VERIFY_LIB, 'obs_box_baseline_verdict <<<"$1" || true', (facts,))
    assert r.returncode == 0 and r.stderr == "", r.stderr
    return [tuple(ln.split("|", 2)) for ln in r.stdout.splitlines()]


def _sink(facts):
    hits = [(st, d) for n, st, d in _rows(facts) if n == "igpu-sink"]
    assert len(hits) == 1
    return hits[0]


def test_the_verdict_fails_the_installed_ddx_on_a_discrete_nvidia_box_right_after_gpu():
    names = [n for n, _, _ in _rows("gather_done=1\ndgpu=1\namdgpu_ddx=\n")]
    assert names[names.index("gpu") + 1] == "igpu-sink"
    st, d = _sink("gather_done=1\ndgpu=1\namdgpu_ddx=install ok installed\n")
    assert st == "FAIL" and DDX in d and "10-amdgpu.conf" in d, d
    for removed in ("absent", "deinstall ok config-files", "unknown ok not-installed"):
        st, d = _sink("gather_done=1\ndgpu=1\namdgpu_ddx=%s\n" % removed)
        assert st == "OK" and "modesetting" in d, (removed, d)
    for present in ("install ok half-installed", "install ok unpacked"):
        assert _sink("gather_done=1\ndgpu=1\namdgpu_ddx=%s\n" % present)[0] == "FAIL", present
    st, d = _sink("gather_done=1\ndgpu=1\namdgpu_ddx=\n")
    assert st == "FAIL" and "unread" in d, "an unread fact is never a pass: " + d


def test_an_igpu_only_box_is_not_graded_on_the_ddx():
    st, d = _sink("gather_done=1\ndgpu=0\namdgpu_ddx=install ok installed\n")
    assert st == "OK" and "no discrete NVIDIA" in d, d


def test_an_incomplete_gather_fails_the_row():
    assert _sink("dgpu=1\namdgpu_ddx=\n")[0] == "FAIL"


def test_the_operator_row_names_the_same_package():
    r = _bash(BASELINE, '. "$1"; [ "$STRIH_OPERATOR_AMDGPU_DDX_PKG" = "$(obs_box_amdgpu_ddx_package)" ]',
              (str(OPERATOR_LIB),))
    assert r.returncode == 0, r.stdout + r.stderr
