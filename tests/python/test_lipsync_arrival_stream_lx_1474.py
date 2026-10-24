"""issue 1474 item 2 -- `scripts/lipsync-test-mode.sh`'s speech-arrival verify on the LINUX stream box.

The verify records a short probe on stream OBS, pulls it to dev1 and deletes it on the box. It used
`win_ssh_download` + a PowerShell `Remove-Item` against the stream box, which cannot reach the Linux
stream-snv (22.10.2026). It now goes through the ONE issue-1462 transport, `stream_box_download` +
`stream_box_exec` with both texts (audio_preflight_delete_ps / audio_preflight_delete_sh), so the box's
obs-fleet class picks plain scp/ssh or the Windows path; the Windows text stays byte for byte (an
OBS_FLEET override, and the two Rust harnesses, whose STREAM address is not in the fleet list).

The real script runs from a scratch copy with its real libs (the Rust harnesses' shape) and a fake
sshpass that logs every call, copies an scp source from this host's filesystem (the "box") and RUNS a
bash remote text sent to the stream box (so the delete is proven by the file being gone). cam2 is a
non-fleet address the fake never runs anything for. Tier-0: no rig.
"""
import base64
import os
import pathlib
import shutil
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
LIBS = ("rig-test-ledger.sh", "win-ssh-exec.sh", "audio-presence-preflight.sh",
        "stream-box.sh", "shell-quote.sh", "obs-fleet.sh")
STREAM = "10.77.9.204"
WIN_STREAM_FLEET = "strih-snv|10.77.9.202|linux-genlock|always\nstream-snv|10.77.9.204|windows-genlock|always"

FAKE_SSHPASS = r'''#!/usr/bin/env bash
printf '%s\n' "$(printf ' %q' "$@")" >> "$FAKE_LOG"
if [ "${3:-}" = scp ]; then
  src="${@: -2:1}"; dst="${!#}"
  case "$src" in
    *@*:*) cp -- "${src#*:}" "$dst" || exit 1 ;;
  esac
  exit 0
fi
case "$*" in
  *"@${FAKE_STREAM}"*) [ -n "${FAKE_RUN_STREAM:-}" ] && exec /bin/bash -c "${!#}" ;;
esac
exit 0
'''


def _rig(tmp_path, rec):
    d = tmp_path / "scripts"
    (d / "lib").mkdir(parents=True)
    (tmp_path / "bin").mkdir()
    shutil.copy(REPO / "scripts" / "lipsync-test-mode.sh", d / "lipsync-test-mode.sh")
    for lib in LIBS:
        shutil.copy(REPO / "scripts" / "lib" / lib, d / "lib" / lib)
    (d / "rig-mode.sh").write_text('#!/usr/bin/env bash\necho "RIG-MODE-TEST-RESTORE $*"\n')
    (d / "rig-mode.sh").chmod(0o755)
    (d / "obs_phase2.py").write_text(
        "import sys\nargv = sys.argv\n"
        "action = argv[argv.index('--action') + 1] if '--action' in argv else ''\n"
        "if action == 'stop':\n    print(%r)\n" % rec)
    (d / "lipsync_envelope_corr.py").write_text("print('corr=0.9')\n")
    sp = tmp_path / "bin" / "sshpass"
    sp.write_text(FAKE_SSHPASS)
    sp.chmod(0o755)
    (tmp_path / "asset.mp4").write_bytes(b"dummy")
    return d


def _run(tmp_path, rec, env_extra):
    d = _rig(tmp_path, rec)
    log = tmp_path / "ssh.log"
    env = {k: v for k, v in os.environ.items() if k not in ("OBS_FLEET", "OBS_FLEET_ALIASES")}
    env.update({"PATH": "%s:%s" % (tmp_path / "bin", env["PATH"]), "FAKE_LOG": str(log),
                "FAKE_STREAM": STREAM, "STREAM": STREAM, "STREAM_USER": "op", "STREAM_PW": "pw",
                "PAINTER_IP": "192.0.2.62", "CAM_PW": "x", "OBS_PASSWORD": "",
                "LIPSYNC_ARRIVAL_RETRIES": "1", "LIPSYNC_ARRIVAL_PROBE_S": "0",
                "LIPSYNC_ARRIVAL_READ_RETRY_SLEEP": "0", "LIPSYNC_ARRIVAL_SSH_TIMEOUT": "10"})
    env.update(env_extra)
    r = subprocess.run(["bash", str(d / "lipsync-test-mode.sh"), "start", str(tmp_path / "asset.mp4")],
                       env=env, capture_output=True, text=True, timeout=120)
    return r, (log.read_text().splitlines() if log.exists() else [])


def _ps(line):
    words = line.replace("\\ ", " ").split()
    i = words.index("-EncodedCommand")
    return base64.b64decode(words[i + 1].strip("'\"")).decode("utf-16-le")


def test_a_linux_stream_box_pulls_and_deletes_the_probe_over_plain_ssh(tmp_path):
    box = tmp_path / "box-home"
    box.mkdir()
    rec = box / "2026-10-24 10-00-00.mkv"
    rec.write_bytes(b"probe")
    r, log = _run(tmp_path, str(rec), {"FAKE_RUN_STREAM": "1"})
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "arrival attempt 1/1: envelope corr=0.9" in out, out
    stream = [ln for ln in log if "@%s" % STREAM in ln]
    assert stream and not [ln for ln in stream if "EncodedCommand" in ln], stream
    pulls = [ln for ln in stream if ln.split()[2:3] == ["scp"]]
    assert len(pulls) == 1 and "UserKnownHostsFile=/dev/null" in pulls[0], pulls
    # the pull's source is the box's StopRecord path (never the local temp file), review round 1
    assert pulls[0].replace("\\ ", " ").split()[-2] == "op@%s:%s" % (STREAM, rec) or \
        ("op@%s:%s" % (STREAM, str(rec).replace(" ", "\\ "))) in pulls[0], pulls
    assert not rec.exists(), "the probe recording is deleted on the box (the bash rm ran)"


def test_a_windows_stream_box_keeps_the_powershell_delete(tmp_path):
    rec = "C:\\Users\\newlevel\\Videos\\2026-10-24 10-00-00.mkv"
    r, log = _run(tmp_path, rec, {"OBS_FLEET": WIN_STREAM_FLEET})
    assert r.returncode == 0, r.stdout + r.stderr
    deletes = [_ps(ln) for ln in log if "@%s" % STREAM in ln and "EncodedCommand" in ln]
    assert len(deletes) == 1 and "Remove-Item" in deletes[0] and rec in deletes[0], deletes


def test_the_two_rust_harnesses_copy_every_lib_the_script_sources():
    for f in ("harness_lipsync_arrival_verify_1192.rs", "harness_lipsync_trap_cleanup_1194.rs"):
        text = (REPO / "tests" / f).read_text()
        for lib in LIBS:
            assert '"%s"' % lib in text, (f, lib)
