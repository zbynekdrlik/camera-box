"""#847 -- imag-nb recording never starts: RecEncoder hardcoded to NVENC, box has no NVIDIA GPU.

Root cause (confirmed live on 10.77.9.187, 2026-07-28): `seed_profile()`'s
`(\"AdvOut\", \"RecEncoder\", \"obs_nvenc_h264_tex\")` was written for the incumbent box's RTX 5050
(#502). The replacement notebook (#816) is Intel iGPU only -- NVENC never initializes ("Encoder ID
'obs_nvenc_h264_tex' not found" in the OBS log), so every StartRecord silently produces 0 bytes.

Fallback investigated LIVE (not assumed, per the #841 TearFree lesson): QSV (`obs_qsv11_v2`) is
listed as loaded in the OBS log, but 3 rounds of live testing on 10.77.9.187 proved it does NOT
actually record on this box/build (render-group + missing oneVPL runtime fixed the first two
failures, then a genuine libmfx Texture/VAAPI-interop MFX_ERR_UNSUPPORTED at Init() -- see the
#847 design comment on the issue for the full trail). `obs_x264` (software) IS live-proven to
record correctly (StartRecord -> outputActive=True, bytes growing, a real playable .mkv) with
ample CPU headroom on the box's isolated cores. So the pure decision is: dGPU present -> NVENC
(byte-for-byte unchanged for a box that still has one), no dGPU -> x264 (the only fallback that
actually works here, NOT QSV).

Hardware detection mirrors `imag_has_discrete_nvidia` (scripts/setup-imag.sh) EXACTLY -- the SAME
regex, since that bash function cannot be imported from Python -- parity-tested against the
IDENTICAL lspci fixture text tests/setup_imag_hardware_agnostic.rs already uses for the bash
detector, so a future drift in either regex is caught by both suites.
"""
import importlib.util
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _scenes_module():
    return _load(REPO / "scripts" / "imag_scenes.py", "imag_scenes_rec_encoder_under_test")


# The SAME two lspci fixtures tests/setup_imag_hardware_agnostic.rs uses for the bash
# `imag_has_discrete_nvidia` detector (nvidia_presence_is_detected_from_lspci_not_assumed) --
# shared source-of-truth text, not independently invented, so both suites prove the same thing.
LSPCI_WITH_DISCRETE_NVIDIA = (
    "00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [8086:a7a8]\n"
    "01:00.0 3D controller [0302]: NVIDIA Corporation GB207M [GeForce RTX 5050] [10de:2dd8]\n"
)
LSPCI_IGPU_ONLY = (
    "00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [8086:a7a8]\n"
    "00:06.2 PCI bridge [0604]: Intel Corporation Device [8086:a73d]\n"
)
# The imag-nb replacement notebook's OWN real `lspci -nn` output (live-captured 2026-07-28,
# 10.77.9.187) -- a "Display controller" class line (not "VGA compatible controller"), proving
# the detector's 3-way class alternation (vga compatible controller|3d controller|display
# controller) matters, not just the two Rust-fixture shapes above.
LSPCI_187_LIVE_CAPTURE = (
    "00:02.0 Display controller [0380]: Intel Corporation Alder Lake-S [UHD Graphics] "
    "[8086:468b] (rev 0c)\n"
)


def test_select_rec_encoder_nvenc_when_discrete_nvidia_present():
    mod = _scenes_module()
    assert mod.select_rec_encoder(True) == "obs_nvenc_h264_tex"


def test_select_rec_encoder_vaapi_tex_when_no_discrete_nvidia():
    """#1143 SUPERSEDES the #847 x264 decision: the no-dGPU (Intel iGPU) choice is now the HARDWARE
    encoder ffmpeg_vaapi_tex -- live-proven to record valid H.264 High 1080p60 while holding render
    at ~4ms/~0% lagged, eliminating the x264 observer effect (#1130). QSV stays NEVER-chosen (#847
    live-proved it broken here); x264 is only the graceful fallback when VAAPI is unavailable
    (tests/python/test_imag_record_encoder_1143.py covers the available-set branches)."""
    mod = _scenes_module()
    assert mod.select_rec_encoder(False) == "ffmpeg_vaapi_tex"


def test_select_rec_encoder_never_returns_qsv():
    """Explicit negative guard: no code path may ever pick obs_qsv11_v2/obs_qsv11_hevc here --
    live-proven unreliable, not merely 'not chosen this time'."""
    mod = _scenes_module()
    for has_dgpu in (True, False):
        assert "qsv" not in mod.select_rec_encoder(has_dgpu).lower()


def test_has_discrete_nvidia_from_lspci_detects_a_real_dgpu():
    mod = _scenes_module()
    assert mod.has_discrete_nvidia_from_lspci(LSPCI_WITH_DISCRETE_NVIDIA) is True


def test_has_discrete_nvidia_from_lspci_rejects_igpu_only():
    mod = _scenes_module()
    assert mod.has_discrete_nvidia_from_lspci(LSPCI_IGPU_ONLY) is False


def test_has_discrete_nvidia_from_lspci_matches_the_187_live_capture():
    mod = _scenes_module()
    assert mod.has_discrete_nvidia_from_lspci(LSPCI_187_LIVE_CAPTURE) is False


def test_has_discrete_nvidia_from_lspci_agrees_with_the_bash_detector():
    """Shared-source-of-truth parity check: run the REAL bash `imag_has_discrete_nvidia` (sourced
    from scripts/setup-imag.sh, its BASH_SOURCE guard skips the destructive flow) against the SAME
    fixtures and assert the Python mirror agrees on every one -- catches drift in EITHER regex."""
    import subprocess

    mod = _scenes_module()
    setup_imag = REPO / "scripts" / "setup-imag.sh"
    for fixture, expect in (
        (LSPCI_WITH_DISCRETE_NVIDIA, True),
        (LSPCI_IGPU_ONLY, False),
        (LSPCI_187_LIVE_CAPTURE, False),
    ):
        proc = subprocess.run(
            ["bash", "-c", f"source {setup_imag}; printf '%s' \"$1\" | imag_has_discrete_nvidia",
             "--", fixture],
            capture_output=True, text=True, timeout=10,
        )
        bash_says = proc.returncode == 0
        assert bash_says == expect, (
            f"bash imag_has_discrete_nvidia disagrees with the expected fixture verdict "
            f"(rc={proc.returncode}, stderr={proc.stderr!r})"
        )
        assert mod.has_discrete_nvidia_from_lspci(fixture) == bash_says, (
            "Python has_discrete_nvidia_from_lspci must agree with the bash detector on the "
            f"SAME lspci text -- fixture={fixture!r}"
        )


class FakeProfileObs:
    """Collects req() calls -- models GetProfileList/GetProfileParameter enough for seed_profile()
    to run to completion without a real OBS."""

    def __init__(self):
        self.calls = []

    def req(self, rtype, payload=None, ignore_err=False):
        self.calls.append((rtype, payload or {}))
        if rtype == "GetProfileList":
            return {"currentProfileName": "imag-60fps"}
        if rtype == "GetProfileParameter":
            return {"parameterValue": "Advanced"}
        return {}


def _rec_encoder_calls(obs):
    return [
        payload["parameterValue"]
        for rtype, payload in obs.calls
        if rtype == "SetProfileParameter"
        and payload.get("parameterCategory") == "AdvOut"
        and payload.get("parameterName") == "RecEncoder"
    ]


def test_seed_profile_sets_nvenc_when_discrete_nvidia_present():
    mod = _scenes_module()
    obs = FakeProfileObs()
    mod.seed_profile(obs, has_discrete_nvidia=True)
    assert _rec_encoder_calls(obs) == ["obs_nvenc_h264_tex"]


def test_seed_profile_sets_vaapi_tex_when_no_discrete_nvidia():
    """#1143: seed_profile seeds the VAAPI-tex default on the no-dGPU box (available=None -> trust
    the Intel bundle). The recordEncoder.json CQP settings are NOT written here (OBS is up during a
    seed -- a clean-shutdown save would clobber them); the E2E ensure-rec-encoder step writes them
    while OBS is DOWN and restarts to make VAAPI live (#847 restart rule)."""
    mod = _scenes_module()
    obs = FakeProfileObs()
    mod.seed_profile(obs, has_discrete_nvidia=False)
    assert _rec_encoder_calls(obs) == ["ffmpeg_vaapi_tex"]


def test_is_local_host_recognizes_loopback_forms():
    mod = _scenes_module()
    assert mod._is_local_host("127.0.0.1") is True
    assert mod._is_local_host("localhost") is True
    assert mod._is_local_host("::1") is True
    assert mod._is_local_host("10.77.9.187") is False


def test_detect_has_discrete_nvidia_local_fails_loud_when_lspci_missing(monkeypatch):
    """#833 class: a missing `lspci` on the machine actually being queried must never be silently
    read as 'no discrete GPU' -- it must fail loud, naming the tool."""
    mod = _scenes_module()
    monkeypatch.setattr(mod.shutil, "which", lambda _name: None)
    try:
        mod.detect_has_discrete_nvidia("127.0.0.1")
        raised = False
    except SystemExit as exc:
        raised = True
        assert "lspci" in str(exc)
    assert raised, "a missing local lspci must sys.exit, never silently proceed"


def test_detect_has_discrete_nvidia_remote_fails_loud_when_lspci_missing(monkeypatch):
    mod = _scenes_module()

    class FakeMissingProbe:
        stdout = "LSPCI_MISSING\n"
        returncode = 0

    monkeypatch.setattr(mod.subprocess, "run", lambda *a, **k: FakeMissingProbe())
    try:
        mod.detect_has_discrete_nvidia("10.77.9.187")
        raised = False
    except SystemExit as exc:
        raised = True
        assert "lspci" in str(exc)
    assert raised, "a missing remote lspci must sys.exit, never silently proceed"


def test_detect_has_discrete_nvidia_remote_parses_real_lspci_text(monkeypatch):
    """End-to-end (with subprocess.run faked): the remote path probes lspci presence, then
    queries it, and the result feeds has_discrete_nvidia_from_lspci correctly."""
    mod = _scenes_module()
    calls = []

    class FakeResult:
        def __init__(self, stdout):
            self.stdout = stdout
            self.returncode = 0

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        joined = " ".join(cmd)
        if "command -v lspci" in joined:
            return FakeResult("LSPCI_OK\n")
        if joined.endswith("lspci -nn") or "lspci -nn" in joined:
            return FakeResult(LSPCI_IGPU_ONLY)
        return FakeResult("")

    monkeypatch.setattr(mod.subprocess, "run", fake_run)
    assert mod.detect_has_discrete_nvidia("10.77.9.187") is False
    assert len(calls) == 2, "must preflight lspci presence BEFORE trusting its output (#833 class)"
