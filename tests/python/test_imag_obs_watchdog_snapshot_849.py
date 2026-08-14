"""#849 -- imag-obs-watchdog.py's tier-b wedge forensic `snapshot()` unconditionally shelled
`nvidia-smi` (x3) + `fuser /dev/nvidia*` and hardcoded PCI address `0000:01:00.0` / `01:00.0`.
The imag box is now Intel iGPU-only (i915, #816) -- so a wedge autopsy produced useless NVIDIA
`[cmd failed]` noise and queried the WRONG PCI slot, mislabelling an unrelated device's link
state as the GPU's. Same "incumbent NVIDIA box" class as #845 (headroom preflight) / #847
(RecEncoder).

The fix makes `snapshot()` hardware-aware, mirroring the #845/#847 convention EXACTLY:
  - detector: the SAME `imag_has_discrete_nvidia` regex setup-imag.sh + imag_scenes.py use
    (parity-tested here against the IDENTICAL lspci fixtures, never a third differently-behaved
    detector).
  - discrete NVIDIA present -> the original nvidia-smi forensics, but with the PCI address
    DERIVED from lspci (never the hardcoded 01:00.0).
  - no discrete GPU -> the LIVE-VERIFIED i915 surface (globbed card*/gt/gt* rps_*_freq_mhz +
    throttle_reason_* [the #1040 PL1-clamp discriminator] + RAPL-mmio package-0 + fuser
    /dev/dri). intel_gpu_top is deliberately EXCLUDED -- it core-dumps on this box
    (intel-gpu-tools 1.28, get_num_gts assertion), so including it would be the #841
    invent-by-analogy trap.
"""
import importlib.util
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _watchdog():
    return _load(REPO / "scripts" / "imag-obs-watchdog.py", "imag_obs_watchdog_under_test")


def _scenes():
    return _load(REPO / "scripts" / "imag_scenes.py", "imag_scenes_parity_under_test")


# The SAME two lspci fixtures tests/setup_imag_hardware_agnostic.rs and
# tests/python/test_imag_scenes_rec_encoder_847.py use -- shared source-of-truth text.
LSPCI_WITH_DISCRETE_NVIDIA = (
    "00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [8086:a7a8]\n"
    "01:00.0 3D controller [0302]: NVIDIA Corporation GB207M [GeForce RTX 5050] [10de:2dd8]\n"
)
LSPCI_IGPU_ONLY = (
    "00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [8086:a7a8]\n"
    "00:06.2 PCI bridge [0604]: Intel Corporation Device [8086:a73d]\n"
)
# A dGPU at a DIFFERENT slot than the old hardcoded 01:00.0 -- proves the address is DERIVED.
LSPCI_NVIDIA_AT_02 = (
    "00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [8086:a7a8]\n"
    "02:00.0 VGA compatible controller [0300]: NVIDIA Corporation AD107 [10de:2882]\n"
)


# ---------------------------------------------------------------------------------------------
# 1. detector -- parity with setup-imag.sh / imag_scenes.py (never a third detector)
# ---------------------------------------------------------------------------------------------

def test_has_discrete_nvidia_true_on_nvidia_false_on_igpu():
    w = _watchdog()
    assert w.has_discrete_nvidia_from_lspci(LSPCI_WITH_DISCRETE_NVIDIA) is True
    assert w.has_discrete_nvidia_from_lspci(LSPCI_IGPU_ONLY) is False


def test_detector_agrees_with_imag_scenes_copy_on_the_shared_fixtures():
    w = _watchdog()
    s = _scenes()
    for fixture in (LSPCI_WITH_DISCRETE_NVIDIA, LSPCI_IGPU_ONLY, LSPCI_NVIDIA_AT_02):
        assert w.has_discrete_nvidia_from_lspci(fixture) == s.has_discrete_nvidia_from_lspci(
            fixture
        ), f"#849: the watchdog detector must agree with imag_scenes.py's copy on: {fixture!r}"


# ---------------------------------------------------------------------------------------------
# 2. PCI address is DERIVED from lspci, never hardcoded 01:00.0
# ---------------------------------------------------------------------------------------------

def test_nvidia_pci_addr_is_derived_not_hardcoded():
    w = _watchdog()
    assert w.nvidia_pci_addr_from_lspci(LSPCI_WITH_DISCRETE_NVIDIA) == "01:00.0"
    assert w.nvidia_pci_addr_from_lspci(LSPCI_NVIDIA_AT_02) == "02:00.0"
    assert w.nvidia_pci_addr_from_lspci(LSPCI_IGPU_ONLY) is None


def test_intel_display_pci_addr_is_derived():
    w = _watchdog()
    assert w.intel_display_pci_addr_from_lspci(LSPCI_IGPU_ONLY) == "00:02.0"
    assert w.intel_display_pci_addr_from_lspci(LSPCI_WITH_DISCRETE_NVIDIA) == "00:02.0"


# ---------------------------------------------------------------------------------------------
# 3. nvidia_forensic_cmds -- references the DERIVED address, never the hardcoded 01:00.0
# ---------------------------------------------------------------------------------------------

def _joined(cmds):
    return "\n".join(label + " :: " + " ".join(argv) for label, argv, _tmo in cmds)


def test_nvidia_forensic_cmds_use_the_passed_address_not_a_hardcoded_one():
    w = _watchdog()
    text = _joined(w.nvidia_forensic_cmds("02:00.0"))
    assert "nvidia-smi" in text, "#849: dGPU branch must still capture nvidia-smi forensics"
    assert "02:00.0" in text, "#849: must query the DERIVED address"
    assert "0000:02:00.0" in text, "#849: sysfs runtime_status path must use the derived domain addr"
    assert "01:00.0" not in text, "#849: the hardcoded 01:00.0 must be gone from the dGPU branch"


def test_nvidia_forensic_cmds_time_the_performance_query_gsp_rpc_discriminator():
    w = _watchdog()
    text = _joined(w.nvidia_forensic_cmds("01:00.0"))
    assert "PERFORMANCE" in text and "GSP-RPC" in text, (
        "#849: the timed nvidia-smi -q -d PERFORMANCE GSP-RPC hang discriminator must survive"
    )


# ---------------------------------------------------------------------------------------------
# 4. igpu_forensic_cmds -- the live-verified i915 surface, NO nvidia, NO core-dumping tool
# ---------------------------------------------------------------------------------------------

def test_igpu_forensic_cmds_capture_the_verified_i915_surface():
    w = _watchdog()
    text = _joined(w.igpu_forensic_cmds("00:02.0"))
    assert "throttle_reason" in text, "#849: i915 throttle reasons (PL1-clamp discriminator, #1040)"
    assert "rps_act_freq_mhz" in text, "#849: i915 actual GT freq (the clamp signature)"
    assert "intel-rapl-mmio" in text, "#849: RAPL package-0 power envelope (#1040)"
    assert "/dev/dri" in text, "#849: fuser on the i915 render nodes (nvidia /dev/nvidia* equiv)"
    assert "00:02.0" in text, "#849: lspci LnkSta must use the DERIVED Intel display address"


def test_igpu_forensic_cmds_never_shell_nvidia_or_the_broken_intel_gpu_top():
    w = _watchdog()
    text = _joined(w.igpu_forensic_cmds("00:02.0")).lower()
    assert "nvidia" not in text, "#849: the no-dGPU branch must never shell nvidia-smi/nvidia paths"
    assert "intel_gpu_top" not in text, (
        "#849: intel_gpu_top core-dumps on this box (intel-gpu-tools 1.28) -- excluding it is the "
        "#841 live-test-not-analogy discipline"
    )


def test_igpu_globs_drm_cards_never_hardcodes_card1():
    w = _watchdog()
    text = _joined(w.igpu_forensic_cmds("00:02.0"))
    assert "/sys/class/drm/card*/" in text, (
        "#849: DRM card path must be GLOBBED (presenter-drm cardN renumbering hazard), never card1"
    )


# ---------------------------------------------------------------------------------------------
# 5. select_forensic_cmds -- the pure branch snapshot() uses
# ---------------------------------------------------------------------------------------------

def test_select_picks_nvidia_branch_on_a_dgpu_box():
    w = _watchdog()
    label, cmds = w.select_forensic_cmds(LSPCI_WITH_DISCRETE_NVIDIA)
    assert "NVIDIA" in label
    assert "nvidia-smi" in _joined(cmds)


def test_select_picks_i915_branch_on_an_igpu_only_box():
    w = _watchdog()
    label, cmds = w.select_forensic_cmds(LSPCI_IGPU_ONLY)
    assert "i915" in label or "iGPU" in label
    text = _joined(cmds)
    assert "throttle_reason" in text and "nvidia-smi" not in text


# ---------------------------------------------------------------------------------------------
# 6. the SOURCE file no longer hardcodes the NVIDIA address / unconditional nvidia-smi in snapshot
# ---------------------------------------------------------------------------------------------

def test_source_no_longer_hardcodes_the_nvidia_pci_address():
    src = (REPO / "scripts" / "imag-obs-watchdog.py").read_text()
    assert "0000:01:00.0" not in src, "#849: the hardcoded runtime_status PCI address must be gone"
    assert "-s 01:00.0" not in src and "-s\", \"01:00.0" not in src, (
        "#849: the hardcoded lspci -s 01:00.0 must be gone"
    )


def test_snapshot_selects_the_branch_by_lspci():
    src = (REPO / "scripts" / "imag-obs-watchdog.py").read_text()
    snap = src[src.index("def snapshot("):]
    snap = snap[: snap.index("\ndef ", 1)]
    assert "select_forensic_cmds" in snap, (
        "#849: snapshot() must pick its forensics via select_forensic_cmds (lspci-branched)"
    )
    assert not re.search(r'\bsh\(\["nvidia-smi"', snap), (
        "#849: snapshot() must not shell nvidia-smi directly any more -- only via the dGPU branch"
    )
