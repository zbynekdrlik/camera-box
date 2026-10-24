"""issue 1462 (owner ROZHODNUTÉ 6377910161) -- the full-path E2E runs on the LINUX stream box
stream-snv, and its A/V measurement stays BLOCKING.

Part 1: the `[0/8]` version-integrity gate grades a Linux stream box on the Linux facets (`--linux
NAME`, `--strih-linux` kept as its alias for `strih`), never UNKNOWN, and still refuses a real drift;
recording-e2e.sh passes `--linux stream` iff the stream box's fleet class is linux-genlock.

Every gate test runs the REAL scripts/version-integrity-gate.sh (and through it the real
drift-guard.sh engine) against state files shaped like the live stream-snv / strih-snv
`:8899/bundle-state.json` (read 24.10.2026: both serve obs_version / distroav_version / output_fps /
genlock_wall_clock / ndi_input_latency / genlock_build_sha, none of the Windows-only keys). The vendor
pin seams keep every run off the network. Tier-0: no rig, no cargo.
"""
import json
import os
import pathlib
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
GATE = SCRIPTS / "version-integrity-gate.sh"
E2E = SCRIPTS / "recording-e2e.sh"

# A synthetic, well-formed build sha (never a real one: the secret scan reads a 40-hex run as a key).
SHA = "b" * 40
HERMETIC = {**os.environ, "VERSION_INTEGRITY_GATE_VENDOR_NEWEST": SHA, "VERSION_INTEGRITY_GATE_VENDOR_PENDING": ""}

# The live stream-snv / strih-snv bundle-state values the drift-guard compare reads (24.10.2026).
STREAM_LX = {
    "obs_version": "32.2.0",
    "distroav_version": "6.2.1",
    "output_fps": "30",
    "genlock_wall_clock": "1",
    "ndi_input_latency": "NDI 2ME PGM=0,Zaloha kamera=0",
    "genlock_build_sha": SHA,
}
STRIH_LX = dict(STREAM_LX, ndi_input_latency="NDI cam1=0,NDI cam2=0,NDI cam3=0,NDI cam5=0")


def _state(tmp_path, name, obj):
    p = tmp_path / ("state-%s.json" % name)
    p.write_text(json.dumps(obj))
    return p


def _gate(tmp_path, *extra, stream=None, strih=None):
    s = _state(tmp_path, "strih", strih or STRIH_LX)
    t = _state(tmp_path, "stream", stream or STREAM_LX)
    args = ["bash", str(GATE), "--win-state", "strih=%s" % s, "--win-state", "stream=%s" % t,
            "--imag-acked-offline", "issue-1462 fixture has no imag", *extra]
    r = subprocess.run(args, cwd=REPO, env=HERMETIC, capture_output=True, text=True, timeout=120)
    return r.returncode, r.stdout, r.stderr


def _box_block(stdout, name):
    """The rows the gate printed for one --win-state box (its engine block + its OBS-identity rows)."""
    start = stdout.index("-- %s (" % name)
    nxt = stdout.find("\n  -- ", start + 1)
    return stdout[start:nxt if nxt != -1 else len(stdout)]


# --- the gate --------------------------------------------------------------------------------------

def test_a_linux_stream_box_passes_under_linux_stream_with_every_windows_facet_skipped(tmp_path):
    rc, out, err = _gate(tmp_path, "--strih-linux", "--linux", "stream")
    assert rc == 0, out + err
    assert "GATE PASS" in out, out
    block = _box_block(out, "stream")
    for facet in ("ndi_runtime", "distroav_dll_paths", "obs_installs", "port4455_identity", "obs_process_count"):
        rows = [ln for ln in block.splitlines() if ln.strip().startswith(facet + " ")]
        assert rows and all("SKIPPED" in ln for ln in rows), (facet, block)
    assert "UNKNOWN" not in block, block
    # the platform-agnostic facets are still graded on the Linux stream box
    for facet in ("obs_version", "distroav_version", "output_fps", "genlock_wall_clock"):
        assert any(ln.strip().startswith(facet + " ") and " OK " in ln for ln in block.splitlines()), (facet, block)


def test_a_skipped_row_names_the_box_it_skipped(tmp_path):
    _rc, out, _err = _gate(tmp_path, "--strih-linux", "--linux", "stream")
    stream = _box_block(out, "stream")
    strih = _box_block(out, "strih")
    assert "stream is a Linux OBS box" in stream, stream
    assert "strih is a Linux OBS box" in strih, strih
    assert "strih is" not in stream, stream


def test_without_the_flag_a_linux_stream_box_still_refuses_unknown(tmp_path):
    rc, out, err = _gate(tmp_path, "--strih-linux")
    assert rc == 11, out + err
    assert "stream:obs_installs" in err, err


def test_strih_linux_is_the_alias_of_linux_strih(tmp_path):
    a = _gate(tmp_path, "--strih-linux", "--linux", "stream")
    b = _gate(tmp_path, "--linux", "strih", "--linux", "stream")
    assert a == b


def test_a_linux_stream_box_still_refuses_a_real_drift(tmp_path):
    rc, out, err = _gate(tmp_path, "--strih-linux", "--linux", "stream",
                         stream=dict(STREAM_LX, distroav_version="6.0.0"))
    assert rc == 20, out + err
    assert "GATE FAILED" in err, err


def test_a_linux_stream_box_still_refuses_a_genlock_build_mismatch(tmp_path):
    rc, out, err = _gate(tmp_path, "--strih-linux", "--linux", "stream",
                         stream=dict(STREAM_LX, genlock_build_sha="c" * 40))
    assert rc == 20, out + err
    parity = [ln for ln in out.splitlines() if "genlock_parity" in ln]
    assert len(parity) == 1 and "DRIFT" in parity[0] and "stream=" in parity[0], out


def test_linux_needs_a_name_of_a_gated_box(tmp_path):
    rc, out, err = _gate(tmp_path, "--linux")
    assert rc == 1, out + err
    rc, out, err = _gate(tmp_path, "--strih-linux", "--linux", "strem")
    assert rc == 1, out + err
    assert "strem" in err, err


def test_a_windows_stream_box_is_graded_exactly_as_before(tmp_path):
    """No --linux for stream: its Windows-only facets run (and read UNKNOWN on these Linux-shaped
    keys), exactly the pre-1462 output."""
    rc, out, _err = _gate(tmp_path, "--strih-linux")
    block = _box_block(out, "stream")
    assert "SKIPPED" not in block, block
    assert "ndi_runtime          UNKNOWN" in block, block


# --- recording-e2e.sh wiring ----------------------------------------------------------------------

def _e2e():
    return E2E.read_text()


def _gate_block(s):
    start = s.index('if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then\n  "$HERE/version-integrity-gate.sh" \\\n')
    end = s.index("\nfi\n", s.index('\nelse\n"$HERE/version-integrity-gate.sh" \\\n', start)) + len("\nfi\n")
    return start, s[start:end]


def test_the_gate_region_resolves_the_stream_platform_before_the_gate():
    s = _e2e()
    banner = s.index("[0/8] version-integrity gate")
    gate_at, block = _gate_block(s)
    region = s[banner:gate_at]
    assert 'stream_box_platform "$STREAM"' in region, region
    assert 'STREAM_LINUX_GATE_ARG="1"' in region, region
    assert block.count("${STREAM_LINUX_GATE_ARG:+--linux stream}") == 2, block


def _run_block(tmp_path, acked, stream_linux):
    _, block = _gate_block(_e2e())
    here = tmp_path / "here"
    here.mkdir(exist_ok=True)
    stub = here / "version-integrity-gate.sh"
    stub.write_text('#!/usr/bin/env bash\nfor a in "$@"; do echo "GATEARG=$a"; done\n')
    stub.chmod(0o755)
    script = (
        "set -euo pipefail\n"
        f'HERE="{here}"\n'
        f'IMAG_OFFLINE_ACKED="{1 if acked else 0}"\n'
        'IMAG_OFFLINE_ACK_REASON="acked"\n'
        'AUTO_WIN_MANIFEST="" AUTO_WIN_ALT_MANIFEST="" AUTO_IMAG_MANIFEST="" IMAG_SO_CSV=""\n'
        'VERSION_STRIH_STATE=/s.json VERSION_STREAM_STATE=/t.json IMAG_GENLOCK_SHA=abc\n'
        'STRIH_LINUX_GATE_ARG="1"\n'
        + ('STREAM_LINUX_GATE_ARG="1"\n' if stream_linux else 'STREAM_LINUX_GATE_ARG=""\n')
        + f"{block}"
        'echo "BLOCK-DONE"\n'
    )
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "BLOCK-DONE" in r.stdout and "unbound variable" not in r.stderr, r.stdout + r.stderr
    return [ln[len("GATEARG="):] for ln in r.stdout.splitlines() if ln.startswith("GATEARG=")]


def test_the_real_gate_block_passes_linux_stream_only_for_a_linux_stream_box(tmp_path):
    for acked in (True, False):
        got = _run_block(tmp_path, acked, True)
        i = got.index("--linux")
        assert got[i:i + 2] == ["--linux", "stream"], got
        assert got[-1] == "--strih-linux", got
        got = _run_block(tmp_path, acked, False)
        assert "--linux" not in got and "stream" not in got, got
        assert got[-1] == "--strih-linux", got
