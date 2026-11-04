"""issue 1387 -- StartRecord / StopRecord on the strih OBS must not cost the JPEG XS program a frame.

Measured on strih-snv 4.11.2026 (bpftrace on libobs + obs-nvenc + libcuda, 11 start/stop cycles):
the TEXTURE NVENC record encoder (`obs_nvenc_hevc_tex`) makes the libobs GL context current on a
FRESH thread at both ends -- the new `obs gpu encode` thread at the start, the output's stop thread
in stop_gpu_encode at the stop -- and that first glXMakeContextCurrent takes 52-60 ms on the NVIDIA
driver while the graphics mutex is held, so the render thread waits ~55 ms: `lagged=1` at 8 of 8
stops, the frame handed twice back to back, `dropped_busy=1` at 7 of 8 stops (and at 3 of 4 E2E
starts). The CUDA (system-memory) NVENC encoder of the same plugin (`obs_nvenc_hevc_soft`, the same
NVENC HEVC hardware and settings, encoding the NV12 program frames libobs already produces for the
raw outputs) takes no GL context at all: 6 of 6 cycles plus a 180 s recording with 0 dropped_busy
and 0 render lags at a stop. So the strih role records with it. The libobs-side cause (a first
MakeCurrent on a fresh thread under the graphics mutex) is the obs project's to fix.
"""
import importlib.util
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"

# The CUDA NVENC HEVC encoder id in the pinned obs release's obs-nvenc plugin (hidden in the encoder
# list -- OBS_ENCODER_CAP_INTERNAL -- yet created by id, and kept by the Settings dialog, which
# inserts a hidden encoder that a profile names).
CUDA_NVENC_HEVC = "obs_nvenc_hevc_soft"


def _profile_module():
    spec = importlib.util.spec_from_file_location("obs_box_stream_profile_1387",
                                                  SCRIPTS / "obs_box_stream_profile.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _profile_facts():
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load strih-snv\n. "$2"\nstrih_lx_profile_facts\n'
    r = subprocess.run(["bash", "-c", script, "h", str(FACTS), str(PROVISION)],
                       capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    return dict(ln.split("=", 1) for ln in r.stdout.split())


def test_the_strih_profile_records_with_the_cuda_nvenc_encoder():
    adv = _profile_module().STRIH_PROFILE["AdvOut"]
    assert adv["RecEncoder"] == CUDA_NVENC_HEVC


def test_the_declared_profile_facts_name_the_cuda_nvenc_encoder():
    assert _profile_facts()["rec_encoder"] == CUDA_NVENC_HEVC


def test_no_strih_declaration_names_a_texture_encoder():
    """A `_tex` record encoder on the strih role puts a fresh-thread GL MakeCurrent under the
    graphics mutex at every StartRecord and StopRecord (the measured ~55 ms render stall)."""
    assert not _profile_module().STRIH_PROFILE["AdvOut"]["RecEncoder"].endswith("_tex")
    assert not _profile_facts()["rec_encoder"].endswith("_tex")
