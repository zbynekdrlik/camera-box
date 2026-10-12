"""issue 1418: the iGPU panel sink of an NVIDIA-primary box runs on the Xorg `modesetting` DDX.

strih-snv 11/12.10.2026: NVIDIA renders the X screen (PRIME nvidia-primary) and the AMD Radeon 780M
iGPU scans the panel out as a reverse-PRIME sink. Xorg autoloaded xf86-video-amdgpu for that sink, and
its panel commit (`commit_work`) blocked forever on an NVIDIA fence: the panel froze for 18 h. The
Intel notebook before it had the same topology with the sink on `modesetting` and was stable
(ROZHODNUTE 6132283178). So the shared baseline (scripts/lib/obs-box-baseline.sh, the NVIDIA/PRIME
step) writes /etc/X11/xorg.conf.d/20-igpu-sink-modesetting.conf on every discrete-NVIDIA box, and the
ONE baseline grader (scripts/lib/obs-box-baseline-verify.sh) grades it in its `igpu-sink` row.
"""

import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
BASELINE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
VERIFY_LIB = REPO / "scripts" / "lib" / "obs-box-baseline-verify.sh"
OPERATOR_LIB = REPO / "scripts" / "lib" / "strih-operator.sh"
CONF = "/etc/X11/xorg.conf.d/20-igpu-sink-modesetting.conf"
SECTION = ['Section "OutputClass"', '    Identifier "igpu-sink-modesetting"', '    MatchDriver "amdgpu"',
           '    Driver "modesetting"', "EndSection"]
ME = "%s:%s" % (subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip(),
                subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip())


def _bash(lib, body, args=()):
    """Run BODY under the callers' strict mode with LIB sourced and a caller-style fail()."""
    script = 'set -euo pipefail\nfail() { echo "FAIL: $1" >&2; exit 1; }\n. "$1"\nshift\n' + body
    return subprocess.run(["bash", "-c", script, "harness", str(lib), *args], capture_output=True,
                          text=True, timeout=120)


def _conf_text():
    r = _bash(BASELINE, "obs_box_igpu_sink_modesetting_conf")
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_the_conf_maps_an_amdgpu_sink_onto_modesetting():
    lines = _conf_text().splitlines()
    i = lines.index(SECTION[0])
    assert lines[i:i + len(SECTION)] == SECTION
    assert all(ln.startswith("#") for ln in lines[:i]), "only comment lines before the section"
    assert "commit_work" in _conf_text() and "modesetting" in _conf_text()
    assert _bash(BASELINE, "obs_box_igpu_sink_conf_path").stdout == CONF


def test_the_install_writes_the_conf_once_and_repairs_a_drift(tmp_path):
    dest = tmp_path / "xorg.conf.d" / "20-igpu-sink-modesetting.conf"
    r = _bash(BASELINE, 'obs_box_igpu_sink_modesetting "$1" "$2"', (str(dest), ME))
    assert r.returncode == 0, r.stderr
    assert "written" in r.stdout and dest.read_text() == _conf_text()
    assert oct(dest.stat().st_mode & 0o777) == "0o644"
    r = _bash(BASELINE, 'obs_box_igpu_sink_modesetting "$1" "$2"', (str(dest), ME))
    assert r.returncode == 0 and "unchanged" in r.stdout, r.stdout + r.stderr
    dest.write_text(dest.read_text().replace('Driver "modesetting"', 'Driver "amdgpu"'))
    r = _bash(BASELINE, 'obs_box_igpu_sink_modesetting "$1" "$2"', (str(dest), ME))
    assert r.returncode == 0 and "written" in r.stdout and dest.read_text() == _conf_text()


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


def test_the_iGPU_only_branch_never_writes_the_conf():
    lib = BASELINE.read_text()
    body = lib[lib.index("\nobs_box_nvidia_prime() {"):]
    body = body[:body.index("\n}\n")]
    call = body.index("    obs_box_igpu_sink_modesetting")
    gate = body.rindex("if lspci -nn | obs_box_has_discrete_nvidia; then", 0, call)
    assert body.index("prime-select nvidia ||", gate) < call < body.index("\nfi\n", gate)
    assert body.count("obs_box_igpu_sink_modesetting") == 1


def _gather(tmp_path, conf_text):
    """The real gather snippet with only the conf path moved into tmp_path; returns igpu_sink_conf."""
    snip = _bash(VERIFY_LIB, 'obs_box_baseline_gather_snippet strih "$1" strih-obs.service',
                 (os.environ.get("USER") or "nobody",))
    assert snip.returncode == 0, snip.stderr
    assert snip.stdout.count(CONF) == 1
    conf = tmp_path / "20-igpu-sink-modesetting.conf"
    if conf_text is not None:
        conf.write_text(conf_text)
    out = subprocess.run(["bash", "-c", snip.stdout.replace(CONF, str(conf))], capture_output=True,
                         text=True, timeout=120).stdout
    vals = [ln.split("=", 1)[1] for ln in out.splitlines() if ln.startswith("igpu_sink_conf=")]
    assert len(vals) == 1, out
    return vals[0]


def test_the_gather_reads_the_installed_conf_against_the_renderer(tmp_path):
    assert _gather(tmp_path, _conf_text()) == "1"
    assert _gather(tmp_path, _conf_text().replace('Driver "modesetting"', 'Driver "amdgpu"')) == "0"
    assert _gather(tmp_path, None) == "0"


def _rows(facts):
    r = _bash(VERIFY_LIB, 'obs_box_baseline_verdict <<<"$1" || true', (facts,))
    assert r.returncode == 0 and r.stderr == "", r.stderr
    return [tuple(ln.split("|", 2)) for ln in r.stdout.splitlines()]


def _sink(facts):
    hits = [(st, d) for n, st, d in _rows(facts) if n == "igpu-sink"]
    assert len(hits) == 1
    return hits[0]


def test_the_verdict_grades_the_conf_on_a_discrete_nvidia_box_right_after_gpu():
    names = [n for n, _, _ in _rows("gather_done=1\ndgpu=1\nigpu_sink_conf=1\n")]
    assert names[names.index("gpu") + 1] == "igpu-sink"
    assert _sink("gather_done=1\ndgpu=1\nigpu_sink_conf=1\n")[0] == "OK"
    st, d = _sink("gather_done=1\ndgpu=1\nigpu_sink_conf=0\n")
    assert st == "FAIL" and CONF in d and "modesetting" in d, d
    assert _sink("gather_done=1\ndgpu=1\n")[0] == "FAIL", "an unread fact is never a pass"


def test_an_igpu_only_box_needs_no_sink_conf():
    st, d = _sink("gather_done=1\ndgpu=0\nigpu_sink_conf=0\n")
    assert st == "OK" and "no discrete NVIDIA" in d, d


def test_an_incomplete_gather_fails_the_row():
    assert _sink("dgpu=1\nigpu_sink_conf=1\n")[0] == "FAIL"


def test_the_operator_row_names_the_same_conf_path():
    assert CONF in OPERATOR_LIB.read_text()
