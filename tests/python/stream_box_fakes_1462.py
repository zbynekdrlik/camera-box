"""issue 1462 -- shared fakes for the stream-box transport tests (not a test module, never collected).

A dev1-side fake `sshpass` that records every call and plays the stream box:
  * `ssh ... user@host "powershell ... -EncodedCommand <b64>"` (the Windows box): the decoded
    PowerShell text is logged and FAKE_WIN_REPLY is printed (nothing is run);
  * `ssh ... user@host "<bash text>"` (the Linux box): the remote text is RUN with /bin/bash on a
    stub-only PATH (FAKE_BOX_BIN), so the test checks what the text does, never only how it reads;
  * `scp ... user@host:<path> <local>`: copies <path> (a file on this host, the box's fs) to <local>.
Every call is one JSON line in FAKE_SSH_LOG. The box tools (`ffmpeg`, `systemctl`) are python stubs
with an absolute shebang (the box PATH holds no python); `rm` is the real one, linked in.
"""
import base64
import json
import os
import sys

_SSHPASS = r'''
import base64, json, os, shutil, subprocess, sys
args = sys.argv[1:]
pw = ""
if args[:1] == ["-p"]:
    pw, args = args[1], args[2:]
tool, rest = args[0], args[1:]
entry = {"tool": tool, "pw": pw, "argv": rest}
if tool == "ssh":
    remote = rest[-1]
    entry["target"] = next((a for a in rest[:-1] if "@" in a), "")
    entry["remote"] = remote
    words = remote.split()
    if words[:1] == ["powershell"] and "-EncodedCommand" in words:
        enc = words[words.index("-EncodedCommand") + 1]
        entry["ps"] = base64.b64decode(enc).decode("utf-16-le")
        with open(os.environ["FAKE_SSH_LOG"], "a") as f:
            f.write(json.dumps(entry) + "\n")
        sys.stdout.write(os.environ.get("FAKE_WIN_REPLY", ""))
        sys.exit(int(os.environ.get("FAKE_WIN_RC", "0")))
    with open(os.environ["FAKE_SSH_LOG"], "a") as f:
        f.write(json.dumps(entry) + "\n")
    env = {"PATH": os.environ["FAKE_BOX_BIN"], "HOME": os.environ.get("FAKE_BOX_HOME", "/nonexistent")}
    for k, v in os.environ.items():
        if k.startswith("FAKE_"):
            env[k] = v
    sys.exit(subprocess.run(["/bin/bash", "-c", remote], env=env).returncode)
if tool == "scp":
    src, dst = rest[-2], rest[-1]
    entry["src"], entry["dst"] = src, dst
    with open(os.environ["FAKE_SSH_LOG"], "a") as f:
        f.write(json.dumps(entry) + "\n")
    path = src.split(":", 1)[1]
    if not os.path.isfile(path):
        sys.stderr.write("scp: %s: No such file or directory\n" % path)
        sys.exit(1)
    shutil.copyfile(path, dst)
    sys.exit(0)
sys.stderr.write("fake sshpass: unknown tool %r\n" % tool)
sys.exit(2)
'''

# The box's ffmpeg: logs its argv; volumedetect prints FAKE_MAX_DB; a pcm_f32le extract writes the
# output file (its last argument); FAKE_FFMPEG_RC is its exit code.
_FFMPEG = r'''
import json, os, sys
with open(os.environ["FAKE_FFMPEG_LOG"], "a") as f:
    f.write(json.dumps(sys.argv[1:]) + "\n")
a = sys.argv[1:]
if "volumedetect" in " ".join(a):
    sys.stdout.write("[Parsed_volumedetect_0 @ 0x1] n_samples: 960000\n")
    sys.stdout.write("[Parsed_volumedetect_0 @ 0x1] max_volume: %s dB\n" % os.environ.get("FAKE_MAX_DB", "-12.3"))
if "pcm_f32le" in a:
    with open(a[-1], "wb") as f:
        f.write(b"RIFF-fake-wav")
sys.exit(int(os.environ.get("FAKE_FFMPEG_RC", "0")))
'''

# The box's systemctl: `--user is-active <unit>` prints FAKE_SVC_STATE (default active).
_SYSTEMCTL = r'''
import os, sys
if "is-active" in sys.argv:
    state = os.environ.get("FAKE_SVC_STATE", "active")
    sys.stdout.write(state + "\n")
    sys.exit(0 if state == "active" else 3)
sys.exit(0)
'''


def _script(path, body):
    path.write_text("#!%s\n%s" % (sys.executable, body))
    path.chmod(0o755)


def make_rig(tmp_path):
    """Build the fake dev1 bin dir (sshpass) + the fake box bin dir. Returns a dict of paths and the
    env to run a driver with (dev1 bin first on PATH)."""
    dev1 = tmp_path / "dev1-bin"
    box = tmp_path / "box-bin"
    dev1.mkdir()
    box.mkdir()
    _script(dev1 / "sshpass", _SSHPASS)
    _script(box / "ffmpeg", _FFMPEG)
    _script(box / "systemctl", _SYSTEMCTL)
    os.symlink("/bin/rm", box / "rm")
    log = tmp_path / "ssh.log"
    ff = tmp_path / "ffmpeg.log"
    env = {k: v for k, v in os.environ.items()
           if k not in ("OBS_FLEET", "OBS_FLEET_ALIASES", "STRIH_BOXES_DIR", "STRIH_PLATFORM", "STRIH_LX_HOST",
                        "STREAM_IP", "STRIH_IP", "OBS_PASSWORD", "OBS_WS_PASSWORD", "CAMERA_SOURCE_BOX")}
    env.update({"PATH": "%s:%s" % (dev1, env.get("PATH", "/usr/bin:/bin")), "FAKE_SSH_LOG": str(log),
                "FAKE_FFMPEG_LOG": str(ff), "FAKE_BOX_BIN": str(box), "HOME": str(tmp_path)})
    return {"dev1": dev1, "box": box, "log": log, "ffmpeg_log": ff, "env": env}


def calls(rig):
    """Every fake sshpass call, in order."""
    if not rig["log"].exists():
        return []
    return [json.loads(line) for line in rig["log"].read_text().splitlines() if line.strip()]


def ffmpeg_calls(rig):
    if not rig["ffmpeg_log"].exists():
        return []
    return [json.loads(line) for line in rig["ffmpeg_log"].read_text().splitlines() if line.strip()]


def ps_encoded(text):
    """The -EncodedCommand payload win_ssh_run sends for TEXT (for a reverse check)."""
    return base64.b64encode(text.encode("utf-16-le")).decode()
