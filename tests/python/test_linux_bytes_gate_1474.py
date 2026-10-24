"""issue 1474 (owner 24.10.2026: "a teda pritvrdil si e2e ci testy aby bola kvalita rovnaka alebo vacsia
ako ked bezal stream/strih na windows?") -- the `[0/8]` version-integrity gate grades the DEPLOYED
library BYTES of a Linux OBS box (strih-snv, stream-snv) against the build's BUNDLE_MANIFEST.json, the
same way the issue-1082 imag facet does, and fails CLOSED on a missing gather or manifest.

On Windows the obs.dll / distroav.dll byte rows were graded; on the Linux boxes they read SKIPPED, so a
deploy of the wrong library never failed the E2E. Every gate test runs the REAL
scripts/version-integrity-gate.sh (and through it drift-guard.sh's `manifest_sha_for_path`) over state
files shaped like the live `:8899/bundle-state.json` of the two boxes. The gather runs the REAL remote
snippet on a fake box (the issue-1462 fake sshpass), the recording-e2e.sh wiring is the REAL gate block.
Tier-0: no rig, no cargo, no network.
"""
import json
import os
import pathlib
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stream_box_fakes_1462 import calls, link_box_tools, make_rig  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
GATE = SCRIPTS / "version-integrity-gate.sh"
LIB = SCRIPTS / "lib" / "manifest-autosource.sh"
HELPER = SCRIPTS / "lib" / "e2e-linux-bytes.sh"
E2E = SCRIPTS / "recording-e2e.sh"

SHA = "b" * 40  # a synthetic build sha (a real 40-hex run reads as a key to the secret scan)
HERMETIC = {**os.environ, "VERSION_INTEGRITY_GATE_VENDOR_NEWEST": SHA, "VERSION_INTEGRITY_GATE_VENDOR_PENDING": ""}

# The six files of the strih-variant linux bundle whose bytes carry our code (read live on strih-snv
# 24.10.2026: each one hashes equal to its BUNDLE_MANIFEST.json entry), manifest-relative.
PATHS = [
    "lib/x86_64-linux-gnu/libobs.so.30",
    "lib/x86_64-linux-gnu/libobs-frontend-api.so.30",
    "lib/x86_64-linux-gnu/obs-plugins/distroav.so",
    "lib/x86_64-linux-gnu/obs-plugins/obs-vban.so",
    "lib/x86_64-linux-gnu/obs-plugins/obs-audio-video-sync-dock.so",
    "bin/obs",
]
HASH = {p: ("%x" % (i + 1)) * 64 for i, p in enumerate(PATHS)}

STREAM_LX = {
    "obs_version": "32.2.0",
    "distroav_version": "6.2.1",
    "output_fps": "30",
    "genlock_wall_clock": "1",
    "ndi_input_latency": "NDI 2ME PGM=0,Zaloha kamera=0",
    "genlock_build_sha": SHA,
}
STRIH_LX = dict(STREAM_LX, ndi_input_latency="NDI cam1=0,NDI cam2=0,NDI cam3=0,NDI cam5=0")


def _manifest(tmp_path, name="bundle_linux_1474", hashes=None, drop=()):
    """A multi-line BUNDLE_MANIFEST.json (drift-guard's parsers are line-based: one files[] entry per line)."""
    hashes = hashes or HASH
    rows = ['    { "path": "%s", "sha256": "%s", "size": 1 }' % (p, h) for p, h in hashes.items() if p not in drop]
    p = tmp_path / ("%s.json" % name)
    p.write_text('{\n  "schema": "camera-box/genlock-bundle-manifest@1",\n  "build_sha": "%s",\n  "files": [\n%s\n  ]\n}\n'
                 % (SHA, ",\n".join(rows)))
    return p


def _csv(hashes=None, drop=()):
    hashes = hashes or HASH
    return ",".join("%s=%s" % (p, h) for p, h in hashes.items() if p not in drop)


def _bytes_args(tmp_path, box, manifest=None, csv=None):
    m = manifest if manifest is not None else _manifest(tmp_path)
    return ["--linux-manifest", "%s=%s" % (box, m), "--linux-bytes", "%s=%s" % (box, _csv() if csv is None else csv)]


def _gate(tmp_path, *extra):
    s = tmp_path / "state-strih.json"
    t = tmp_path / "state-stream.json"
    s.write_text(json.dumps(STRIH_LX))
    t.write_text(json.dumps(STREAM_LX))
    args = ["bash", str(GATE), "--win-state", "strih=%s" % s, "--win-state", "stream=%s" % t,
            "--imag-acked-offline", "issue-1474 fixture has no imag", *extra]
    r = subprocess.run(args, cwd=REPO, env=HERMETIC, capture_output=True, text=True, timeout=120)
    return r.returncode, r.stdout, r.stderr


def _rows(out, box):
    start = out.index("-- %s linux .so byte parity" % box)
    nxt = out.find("\n  -- ", start + 1)
    return out[start:nxt if nxt != -1 else len(out)]


def _clean(tmp_path):
    return [*_bytes_args(tmp_path, "strih"), *_bytes_args(tmp_path, "stream")]


# --- the gate --------------------------------------------------------------------------------------

def test_both_linux_boxes_pass_with_every_library_graded_ok(tmp_path):
    rc, out, err = _gate(tmp_path, *_clean(tmp_path), "--linux", "stream", "--strih-linux")
    assert rc == 0, out + err
    assert "GATE PASS" in out, out
    for box in ("strih", "stream"):
        rows = _rows(out, box)
        oks = [ln for ln in rows.splitlines() if ln.strip().startswith("linux_so_bytes") and " OK " in ln]
        assert len(oks) == len(PATHS), rows
        for p in PATHS:
            assert any(p.rsplit("/", 1)[-1] in ln for ln in oks), (p, rows)
        assert "SKIPPED" not in rows and "UNKNOWN" not in rows, rows


def test_a_linux_box_without_its_byte_gather_refuses_unknown(tmp_path):
    rc, out, err = _gate(tmp_path, *_bytes_args(tmp_path, "strih"), "--linux", "stream", "--strih-linux")
    assert rc == 11, out + err
    assert "stream:so_bytes" in err, err
    assert "UNKNOWN" in _rows(out, "stream"), out


def test_a_missing_manifest_refuses_unknown_never_a_pass(tmp_path):
    rc, out, err = _gate(tmp_path, *_bytes_args(tmp_path, "strih"),
                         "--linux-bytes", "stream=%s" % _csv(), "--linux", "stream", "--strih-linux")
    assert rc == 11, out + err
    assert "stream:so_bytes" in err, err


def test_an_unreadable_manifest_refuses_unknown(tmp_path):
    rc, out, err = _gate(tmp_path, *_bytes_args(tmp_path, "strih"),
                         *_bytes_args(tmp_path, "stream", manifest=tmp_path / "absent.json"),
                         "--linux", "stream", "--strih-linux")
    assert rc == 11, out + err
    assert "not readable" in _rows(out, "stream"), out


def test_a_drifted_library_refuses_and_names_the_file_and_the_box(tmp_path):
    bad = dict(HASH)
    bad["lib/x86_64-linux-gnu/obs-plugins/obs-vban.so"] = "f" * 64
    rc, out, err = _gate(tmp_path, *_bytes_args(tmp_path, "strih"),
                         *_bytes_args(tmp_path, "stream", csv=_csv(bad)), "--linux", "stream", "--strih-linux")
    assert rc == 20, out + err
    rows = _rows(out, "stream")
    drift = [ln for ln in rows.splitlines() if "DRIFT" in ln]
    assert len(drift) == 1 and "obs-vban.so" in drift[0] and "stream" in drift[0], rows


def test_a_library_the_box_did_not_hash_is_unknown_by_name(tmp_path):
    """The gather prints no line for a file it cannot read: the gate names it, never grades the rest as clean."""
    gone = "lib/x86_64-linux-gnu/obs-plugins/obs-audio-video-sync-dock.so"
    rc, out, err = _gate(tmp_path, *_bytes_args(tmp_path, "strih"),
                         *_bytes_args(tmp_path, "stream", csv=_csv(drop=[gone])), "--linux", "stream", "--strih-linux")
    assert rc == 11, out + err
    rows = _rows(out, "stream")
    assert any("UNKNOWN" in ln and gone in ln for ln in rows.splitlines()), rows


def test_a_library_the_manifest_does_not_list_is_unknown(tmp_path):
    gone = "bin/obs"
    rc, out, err = _gate(tmp_path, *_bytes_args(tmp_path, "strih"),
                         *_bytes_args(tmp_path, "stream", manifest=_manifest(tmp_path, "bundle_short_1474", drop=[gone])),
                         "--linux", "stream", "--strih-linux")
    assert rc == 11, out + err
    assert any("UNKNOWN" in ln and gone in ln for ln in _rows(out, "stream").splitlines()), out


def test_a_drift_outranks_an_unknown(tmp_path):
    bad = dict(HASH)
    bad["bin/obs"] = "e" * 64
    rc, out, err = _gate(tmp_path, *_bytes_args(tmp_path, "strih"),
                         *_bytes_args(tmp_path, "stream", csv=_csv(bad, drop=[PATHS[1]])),
                         "--linux", "stream", "--strih-linux")
    assert rc == 20, out + err


def test_byte_args_for_a_box_that_is_not_linux_graded_are_a_usage_error(tmp_path):
    rc, out, err = _gate(tmp_path, *_clean(tmp_path), "--strih-linux")
    assert rc == 1, out + err
    assert "stream" in err, err


def test_a_windows_box_gets_no_linux_byte_row(tmp_path):
    rc, out, _err = _gate(tmp_path, *_bytes_args(tmp_path, "strih"), "--strih-linux")
    assert "-- stream linux .so byte parity" not in out, out
    assert "-- strih linux .so byte parity" in out, out


def test_the_byte_row_runs_per_box_after_the_box_rows(tmp_path):
    _rc, out, _err = _gate(tmp_path, *_clean(tmp_path), "--linux", "stream", "--strih-linux")
    assert out.index("-- strih (") < out.index("-- strih linux .so byte parity") < out.index("-- stream (")
    assert out.index("-- stream (") < out.index("-- stream linux .so byte parity") < out.index("cross-box genlock parity")


# --- the lib: one path list, the gather, the parser ----------------------------------------------

def _lib(body, *args):
    script = 'set -euo pipefail\n. "%s"\n%s\n' % (LIB, body)
    r = subprocess.run(["bash", "-c", script, "lib", *args], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stdout + r.stderr
    return r.stdout


def test_the_lib_names_the_six_bundle_files_once():
    assert _lib("linux_obs_box_byte_paths").split() == PATHS


def test_the_bundle_artifact_is_the_one_the_deploy_installs():
    art = _lib("linux_obs_box_bundle_artifact").strip()
    deploy = subprocess.run(
        ["bash", "-c", 'set -uo pipefail\n. "$1"\nfleet_linux_bundle_artifact_for strih-snv\nfleet_linux_bundle_artifact_for stream-snv',
         "t", str(SCRIPTS / "deploy-genlock-fleet.sh")], capture_output=True, text=True, timeout=30, cwd=REPO)
    assert deploy.returncode == 0, deploy.stdout + deploy.stderr
    assert deploy.stdout.split() == [art, art], deploy.stdout


def test_the_imag_gather_text_is_unchanged():
    want = (
        "if command -v sha256sum >/dev/null 2>&1; then\n"
        "  for _p in lib/x86_64-linux-gnu/libobs.so.30 lib/x86_64-linux-gnu/obs-plugins/distroav.so"
        " lib/x86_64-linux-gnu/libobs-opengl.so.30; do\n"
        '    _f="/usr/$_p"\n'
        '    if [ -r "$_f" ]; then\n'
        "      _h=\"$(sha256sum \"$_f\" 2>/dev/null | awk '{print $1}')\"\n"
        "      [ -n \"$_h\" ] && printf '%s %s\\n' \"$_p\" \"$_h\"\n"
        "    fi\n"
        "  done\n"
        "else\n"
        "  echo TOOL_MISSING:sha256sum\n"
        "fi\n"
    )
    assert _lib("imag_so_gather_cmd") == want


def _fake_root(tmp_path, skip=()):
    root = tmp_path / "box-root"
    for p in PATHS:
        if p in skip:
            continue
        f = root / p
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(p.encode())
    return root


def _real_sha(p):
    import hashlib
    return hashlib.sha256(p.encode()).hexdigest()


def test_the_linux_gather_hashes_every_file_on_the_box_and_the_parser_keeps_bin_and_lib(tmp_path):
    root = _fake_root(tmp_path)
    snippet = _lib('linux_obs_box_so_gather_cmd "$1"', str(root))
    r = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    csv = _lib('linux_obs_box_so_bytes_csv "$1"', r.stdout)
    assert csv == ",".join("%s=%s" % (p, _real_sha(p)) for p in PATHS), csv


def test_an_unreadable_file_is_left_out_of_the_gather(tmp_path):
    root = _fake_root(tmp_path, skip=["bin/obs"])
    snippet = _lib('linux_obs_box_so_gather_cmd "$1"', str(root))
    out = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True, timeout=30).stdout
    assert "bin/obs" not in out and len(out.splitlines()) == len(PATHS) - 1, out


def test_a_missing_sha256sum_yields_no_csv():
    assert _lib('linux_obs_box_so_bytes_csv "$1"', "TOOL_MISSING:sha256sum\n") == ""


def test_the_parser_drops_a_banner_line_and_a_line_without_a_sha():
    out = _lib('linux_obs_box_so_bytes_csv "$1"', "Welcome to stream-snv\nbin/obs\nbin/obs %s\n" % ("a" * 64))
    assert out == "bin/obs=%s" % ("a" * 64), out


# --- the E2E helper: gather + manifest, appended to LINUX_BYTES_GATE_ARGS ---------------------------

def _seam(tmp_path, ok=True):
    """MANIFEST_AUTOSOURCE_CMD fixture: records its args, copies a manifest to DEST (or fails)."""
    rec = tmp_path / "seam.log"
    src = _manifest(tmp_path, "bundle_seam_1474")
    seam = tmp_path / "seam.sh"
    seam.write_text('#!/bin/bash\necho "$* timeout=${MANIFEST_AUTOSOURCE_TIMEOUT_S:-unset}" >> "%s"\n%s\n' % (
        rec, ('cp "%s" "$5" && printf %%s "$5"' % src) if ok else "exit 1"))
    seam.chmod(0o755)
    return seam, rec


def _helper(tmp_path, rig, *, ok_manifest=True, state_sha=SHA, down=False):
    root = _fake_root(tmp_path)
    link_box_tools(rig, "sha256sum", "awk")
    state = tmp_path / "strih-state.json"
    state.write_text(json.dumps(dict(STRIH_LX, genlock_build_sha=state_sha)))
    seam, rec = _seam(tmp_path, ok_manifest)
    out = tmp_path / "out"
    out.mkdir()
    env = dict(rig["env"], MANIFEST_AUTOSOURCE_CMD=str(seam), E2E_LINUX_BYTES_REMOTE_PREFIX=str(root))
    if down:
        env["FAKE_SSH_DOWN"] = "1"
    script = (
        'set -euo pipefail\n'
        '. "%s"\n. "%s"\n'
        'LINUX_BYTES_GATE_ARGS=()\n'
        'e2e_linux_bytes_gather strih 10.77.9.202 newlevel pw "%s" "%s" zbynekdrlik/camera-box\n'
        'printf "ARG=%%s\\n" ${LINUX_BYTES_GATE_ARGS[@]+"${LINUX_BYTES_GATE_ARGS[@]}"}\n'
        'echo HELPER-DONE\n' % (LIB, HELPER, state, out)
    )
    r = subprocess.run(["bash", "-c", script], env=env, capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "HELPER-DONE" in r.stdout, r.stdout + r.stderr
    args = [ln[4:] for ln in r.stdout.splitlines() if ln.startswith("ARG=")]
    return args, r, rec


def test_the_helper_hands_the_gate_the_box_bytes_and_the_ci_manifest(tmp_path):
    rig = make_rig(tmp_path)
    args, r, rec = _helper(tmp_path, rig)
    assert args[0] == "--linux-bytes", args
    assert args[1] == "strih=" + ",".join("%s=%s" % (p, _real_sha(p)) for p in PATHS), args
    assert args[2] == "--linux-manifest" and args[3].startswith("strih="), args
    assert pathlib.Path(args[3].split("=", 1)[1]).read_text() == _manifest(tmp_path, "bundle_seam_1474").read_text()
    seam_args = rec.read_text().split()
    assert seam_args[:4] == ["zbynekdrlik/camera-box", "linux-genlock.yml", "obs-genlock-linux-x86_64-strih", SHA], seam_args
    # the strih bundle is one ~600 MB zip (run 40031379800: 601430199 bytes) that GitHub serves only
    # whole, so its first fetch per build needs more than the 300 s default gh bound
    assert seam_args[-1] == "timeout=900", seam_args
    ssh = [c for c in calls(rig) if c["tool"] == "ssh"]
    assert len(ssh) == 1 and ssh[0]["target"] == "newlevel@10.77.9.202", ssh
    assert "UserKnownHostsFile=/dev/null" in " ".join(ssh[0]["argv"]), ssh


def test_a_failed_manifest_fetch_passes_no_manifest_so_the_gate_refuses(tmp_path):
    rig = make_rig(tmp_path)
    args, r, _rec = _helper(tmp_path, rig, ok_manifest=False)
    assert "--linux-manifest" not in args and "--linux-bytes" in args, args
    assert "manifest" in r.stdout + r.stderr, r.stdout + r.stderr


def test_a_box_that_does_not_answer_passes_no_bytes_and_says_so(tmp_path):
    rig = make_rig(tmp_path)
    args, r, _rec = _helper(tmp_path, rig, down=True)
    assert "--linux-bytes" not in args, args
    assert "strih" in r.stdout + r.stderr, r.stdout + r.stderr


def test_a_state_without_a_build_sha_fetches_no_manifest(tmp_path):
    rig = make_rig(tmp_path)
    args, _r, rec = _helper(tmp_path, rig, state_sha="")
    assert "--linux-manifest" not in args, args
    assert not rec.exists(), rec.read_text()


# --- recording-e2e.sh wiring ------------------------------------------------------------------------

def _gate_block(s):
    start = s.index('if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then\n  "$HERE/version-integrity-gate.sh" \\\n')
    end = s.index("\nfi\n", s.index('\nelse\n"$HERE/version-integrity-gate.sh" \\\n', start)) + len("\nfi\n")
    return start, s[start:end]


EXPANSION = '${LINUX_BYTES_GATE_ARGS[@]+"${LINUX_BYTES_GATE_ARGS[@]}"}'


def test_both_gate_invocations_pass_the_linux_byte_args_before_the_strih_linux_tail():
    s = E2E.read_text()
    _, block = _gate_block(s)
    assert block.count(EXPANSION) == 2, block
    for part in block.split("\nelse\n"):
        assert part.index(EXPANSION) < part.index("${STRIH_LINUX_GATE_ARG:+--strih-linux}"), part


def test_the_gather_runs_for_each_linux_box_before_the_gate():
    s = E2E.read_text()
    gate_at, _ = _gate_block(s)
    banner = s.index("[0/8] version-integrity gate")
    region = s[banner:gate_at]
    assert region.count('. "$HERE/lib/e2e-linux-bytes.sh"') == 1, region
    assert 'LINUX_BYTES_GATE_ARGS=()' in region, region
    strih = 'e2e_linux_bytes_gather strih "$STRIH" "$STRIH_USER" "$STRIH_PW" "$VERSION_STRIH_STATE" "$OUTDIR" "$VERSION_GATE_REPO"'
    stream = 'e2e_linux_bytes_gather stream "$STREAM" "$STREAM_USER" "$STREAM_PW" "$VERSION_STREAM_STATE" "$OUTDIR" "$VERSION_GATE_REPO"'
    assert region.count(strih) == 1 and region.count(stream) == 1, region
    # each only for its own Linux box
    assert region.index('STRIH_LINUX_GATE_ARG="1"') < region.index(strih)
    assert region.index('STREAM_LINUX_GATE_ARG="1"') < region.index(stream)


def _run_block(tmp_path, decl):
    _, block = _gate_block(E2E.read_text())
    here = tmp_path / "here"
    here.mkdir(exist_ok=True)
    stub = here / "version-integrity-gate.sh"
    stub.write_text('#!/usr/bin/env bash\nfor a in "$@"; do echo "GATEARG=$a"; done\n')
    stub.chmod(0o755)
    out = []
    for acked in (1, 0):
        script = (
            "set -euo pipefail\n"
            f'HERE="{here}"\nIMAG_OFFLINE_ACKED="{acked}"\nIMAG_OFFLINE_ACK_REASON="acked"\n'
            'AUTO_WIN_MANIFEST="" AUTO_WIN_ALT_MANIFEST="" AUTO_IMAG_MANIFEST="" IMAG_SO_CSV=""\n'
            'VERSION_STRIH_STATE=/s.json VERSION_STREAM_STATE=/t.json IMAG_GENLOCK_SHA=abc\n'
            'STRIH_LINUX_GATE_ARG="1" STREAM_LINUX_GATE_ARG="1"\n'
            f"{decl}\n{block}"
            'echo "BLOCK-DONE"\n'
        )
        r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30)
        assert r.returncode == 0 and "BLOCK-DONE" in r.stdout, r.stdout + r.stderr
        out.append([ln[len("GATEARG="):] for ln in r.stdout.splitlines() if ln.startswith("GATEARG=")])
    return out


def test_the_real_gate_block_hands_the_array_on_both_branches(tmp_path):
    decl = 'LINUX_BYTES_GATE_ARGS=(--linux-manifest "strih=/o/m x.json" --linux-bytes "strih=bin/obs=aa")'
    for got in _run_block(tmp_path, decl):
        i = got.index("--linux-manifest")
        assert got[i:i + 4] == ["--linux-manifest", "strih=/o/m x.json", "--linux-bytes", "strih=bin/obs=aa"], got
        assert got[-1] == "--strih-linux", got


def test_the_real_gate_block_is_nounset_safe_without_the_array(tmp_path):
    for decl in ("unset LINUX_BYTES_GATE_ARGS", "LINUX_BYTES_GATE_ARGS=()"):
        for got in _run_block(tmp_path, decl):
            assert "--linux-manifest" not in got and "" not in got, got
