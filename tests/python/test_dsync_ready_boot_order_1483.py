"""issue 1483: on a Linux OBS box boot, OBS must never start on a wall clock dsync will still step.

strih-pp, 25.10.2026: OBS came up at 09:06:17, dsync (the PP date master) stepped its own wall back
3.7 s only at 09:07:05, so the running OBS saw a 3.5 s wall step (stream-pp `NDI 2ME PGM` underruns,
`audio_place_err_ms=3506`). It reproduced on every boot.

dsync's half shipped in v1.30.1 (dsync#164; camera-box pins 1.35.2): READY=1 only once the boot date is
placed, bounded at 30 s. The camera-box half (design 6902021067, Approach 1):
- the dsync unit on the Linux OBS boxes is `Type=notify` (+ `NotifyAccess=main`, `TimeoutStartSec=120`,
  the lines of dsync's own installer unit), so systemd knows when the date is placed;
- a lightdm.service drop-in `After=dsync.service` + `Wants=dsync.service` (never `Requires=`: a dead
  clock must never leave the operator a black screen) beside the GPU-ready drop-in, so the kiosk session
  and its `--user` strih-obs unit start after it;
- the shared grader's `kiosk` row FAILs while that drop-in is missing or differs.
The cambox dsync unit is out of scope and stays `Type=simple`.
"""

import os
import stat
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
PROVISION = REPO / "scripts" / "lib" / "strih-provision.sh"
CAMBOX_UNIT_LIB = REPO / "scripts" / "lib" / "cambox-dsync-unit.sh"
BASELINE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
VERIFY_LIB = REPO / "scripts" / "lib" / "obs-box-baseline-verify.sh"
DROPIN_PATH = "/etc/systemd/system/lightdm.service.d/55-obs-box-dsync-ready.conf"
GPU_DROPIN_PATH = "/etc/systemd/system/lightdm.service.d/50-obs-box-gpu-ready.conf"
ME = "%s:%s" % (subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip(),
                subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip())


def _bash(lib, body, args=(), env=None, timeout=60):
    """Run BODY under the callers' strict mode with LIB sourced and a caller-style fail()."""
    script = 'set -euo pipefail\nfail() { echo "FAIL: $1" >&2; exit 1; }\n. "$1"\nshift\n' + body
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "harness", str(lib), *args], capture_output=True,
                          text=True, timeout=timeout, env=e)


def _render(lib, fn):
    r = _bash(lib, fn)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _service_lines(unit):
    """The [Service] section's directive lines (comments dropped)."""
    out, inside = [], False
    for line in unit.splitlines():
        if line.startswith("["):
            inside = line == "[Service]"
            continue
        if inside and line and not line.startswith("#"):
            out.append(line)
    return out


# --- (a) the dsync unit ----------------------------------------------------------------------


def test_the_server_unit_is_type_notify_with_dsyncs_own_start_lines():
    unit = _render(PROVISION, "strih_dantesync_unit_text server '' enp7s0")
    svc = _service_lines(unit)
    assert "Type=notify" in svc, unit
    assert "NotifyAccess=main" in svc, unit
    assert "TimeoutStartSec=120" in svc, unit
    assert not any(line.startswith("Type=") and line != "Type=notify" for line in svc), unit
    # the rest of the shape is unchanged
    for part in ("ExecStart=/usr/local/bin/dsync --interface enp7s0", "Restart=always", "RestartSec=5"):
        assert part in svc, unit
    assert "WantedBy=multi-user.target" in unit


def test_a_client_unit_is_type_notify_too():
    unit = _render(PROVISION, "strih_dantesync_unit_text client '--ntp-server strih-snv.lan' enp56s0")
    svc = _service_lines(unit)
    assert "Type=notify" in svc and "NotifyAccess=main" in svc and "TimeoutStartSec=120" in svc, unit


def test_the_start_timeout_covers_dsyncs_ready_bound():
    """dsync sends READY=1 at the latest 30 s after its loop starts (boot_ready::READY_PLACEMENT_BOUND);
    the start timeout must leave room for that plus the start before it, never cut it."""
    unit = _render(PROVISION, "strih_dantesync_unit_text server '' enp7s0")
    timeout = [int(line.split("=", 1)[1]) for line in _service_lines(unit) if line.startswith("TimeoutStartSec=")]
    assert timeout == [120], unit
    assert timeout[0] > 30


def test_the_cambox_unit_stays_type_simple():
    """Scope = the Linux OBS boxes: the cambox renderer is not this ticket's."""
    unit = _render(CAMBOX_UNIT_LIB, "cambox_dsync_unit_text enp2s0")
    assert "Type=simple" in _service_lines(unit), unit
    assert "Type=notify" not in unit


# --- (b) the lightdm drop-in ---------------------------------------------------------------


def test_the_drop_in_orders_lightdm_after_dsync_and_never_requires_it():
    text = _render(BASELINE, "obs_box_dsync_ready_dropin_text")
    lines = [ln for ln in text.splitlines() if ln and not ln.startswith("#")]
    assert lines == ["[Unit]", "After=dsync.service", "Wants=dsync.service"], text
    assert "Requires=" not in text and "BindsTo=" not in text and "Requisite=" not in text
    assert _render(BASELINE, "obs_box_dsync_ready_dropin_path") == DROPIN_PATH


def test_the_drop_in_sits_beside_the_gpu_ready_one():
    assert DROPIN_PATH.rsplit("/", 1)[0] == GPU_DROPIN_PATH.rsplit("/", 1)[0]
    assert _render(BASELINE, "obs_box_gpu_ready_dropin_path") == GPU_DROPIN_PATH


def test_install_writes_the_drop_in_through_write_if_changed_and_is_idempotent(tmp_path):
    d = tmp_path / "lightdm.service.d" / "55-obs-box-dsync-ready.conf"
    body = 'obs_box_dsync_ready_install "$1" "$2"'
    r = _bash(BASELINE, body, args=(str(d), ME))
    assert r.returncode == 0, r.stderr
    assert d.read_text() == _render(BASELINE, "obs_box_dsync_ready_dropin_text")
    assert stat.S_IMODE(d.stat().st_mode) == 0o644
    assert "written" in r.stdout
    again = _bash(BASELINE, body, args=(str(d), ME))
    assert again.returncode == 0, again.stderr
    assert "written" not in again.stdout and again.stdout.count("unchanged") == 1


def test_install_keeps_the_drop_in_when_its_renderer_fails(tmp_path):
    d = tmp_path / "55-obs-box-dsync-ready.conf"
    d.write_text("kept\n")
    r = _bash(BASELINE, 'obs_box_dsync_ready_dropin_text() { printf "[Unit]\\n"; return 2; }\n'
              'obs_box_dsync_ready_install "$1" "$2" || echo "RC=$?"', args=(str(d), ME))
    assert d.read_text() == "kept\n"
    assert "FAIL:" in r.stderr or "RC=" in r.stdout, (r.stdout, r.stderr)


def test_the_kiosk_item_installs_the_drop_in_after_the_gpu_wait():
    r = subprocess.run(["bash", "-c", '. "$1"; declare -f obs_box_kiosk', "h", str(BASELINE)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    body = r.stdout
    assert "obs_box_dsync_ready_install" in body
    assert body.index("obs_box_gpu_ready_install") < body.index("obs_box_dsync_ready_install")


# --- the grader -----------------------------------------------------------------------------


def _gather_text(tmp_path):
    snippet = subprocess.run(
        ["bash", "-c", '. "$1"; obs_box_baseline_gather_snippet strih newlevel strih-obs.service', "h",
         str(VERIFY_LIB)], capture_output=True, text=True, timeout=30)
    assert snippet.returncode == 0, snippet.stderr
    text = snippet.stdout
    assert text.count(DROPIN_PATH) == 1, \
        "the gather must read the path through the ONE embedded path function, never retype it"
    d = tmp_path / "dsync-dropin"
    stub = f'obs_box_dsync_ready_dropin_path() {{ printf %s "{d}"; }}\n'
    # the embedded path function comes first in the snippet; the stub redefines it after
    return d, text.replace("set +e\n", "set +e\n" + stub, 1)


def _run_gather(text):
    r = subprocess.run(["bash", "-c", text], capture_output=True, text=True, timeout=120)
    facts = {}
    for line in r.stdout.splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            facts[k] = v
    return facts


def test_the_gather_reads_the_installed_drop_in(tmp_path):
    d, text = _gather_text(tmp_path)
    assert _run_gather(text)["dsync_ready"] == "0", "a missing drop-in is never installed"
    d.write_text(_render(BASELINE, "obs_box_dsync_ready_dropin_text"))
    assert _run_gather(text)["dsync_ready"] == "1"
    d.write_text(_render(BASELINE, "obs_box_dsync_ready_dropin_text").replace("Wants=", "Requires="))
    assert _run_gather(text)["dsync_ready"] == "0", "a hand-edited drop-in differs"


def _kiosk_row(dsync_ready):
    facts = "\n".join([
        "gather_done=1", "lightdm=install ok installed", "openbox=install ok installed",
        "dm=/usr/lib/systemd/system/lightdm.service", "dm_lightdm=/usr/lib/systemd/system/lightdm.service",
        "autologin=1", "gdm3=", "gnome_shell=", "gpu_wait=1", f"dsync_ready={dsync_ready}", "x_core=0"]) + "\n"
    r = subprocess.run(["bash", "-c", '. "$1"; obs_box_baseline_verdict', "h", str(VERIFY_LIB)],
                       input=facts, capture_output=True, text=True, timeout=30)
    rows = [line for line in r.stdout.splitlines() if line.startswith("kiosk|")]
    assert len(rows) == 1, r.stdout
    return rows[0]


def test_the_kiosk_row_needs_the_dsync_ready_drop_in():
    assert _kiosk_row("1").startswith("kiosk|OK|")
    missing = _kiosk_row("0")
    assert missing.startswith("kiosk|FAIL|") and "dsync-ready order installed=0" in missing
    assert _kiosk_row("").startswith("kiosk|FAIL|"), "an unread fact is never a pass"
