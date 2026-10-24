"""Issue 1404 -- fakes for the full-path E2E's YouTube stage tests (not a test module).

`Fakes(tmp_path)` writes three executables (session, obs, guard) that log every call to calls.log
and answer from per-kind scripts: `fakes.script("session", "start", rc, {json})` queues one answer for
that subcommand (the last queued answer repeats). `fakes.env` points the orchestrator's
YOUTUBE_LEG_*_CMD seams at them.

The measurement host (design 6387919956 part c) is a fake `ssh` (YOUTUBE_LEG_SSH_CMD) that RUNS the
remote bash for real, with HOME = `fakes.home` (the fake dev2) and the remote PATH the orchestrator
sets (`$HOME/.local/bin` first), where two stubs stand in for the host's tools:
  - `python3` = the verdict tool: `--prime-ticks` (kind "prime") writes a tick map into --out and
    exits with the queued rc; any other call (kind "verdict") writes the queued JSON as
    <--out>/youtube-leg-verdict.json + a VOD tick map and exits with the queued rc. A queued
    `{"sleep": s}` answer sleeps s seconds first (a decode that runs over its bound);
  - `sshpass` = the pull from the stream box: it copies `fakes.stream/<path>` to the destination,
    logs whether SSHPASS arrived (never the value) and whether the password leaked into its argv;
    `fail_one_pull(rc)` makes ONE pull fail with that exit code (scp's 255 = stream box unreachable).
`fakes.host_down()` makes ssh exit 255 (the host unreachable). Real timeout/tar/find/nice run.
"""
import json
import os
import sys
from pathlib import Path

FAKE = r'''#!{py}
import json, os, sys
kind = {kind!r}
d = {d!r}
argv = sys.argv[1:]
with open(os.path.join(d, "calls.log"), "a") as f:
    f.write(json.dumps([kind] + argv) + "\n")
marker = os.environ.get("YOUTUBE_LEG_MARKER")
if kind == "obs" and marker and os.path.exists(marker):
    open(os.path.join(d, "marker-at-" + argv[-1] + ".json"), "w").write(open(marker).read())
sub = {{"session": lambda a: next((x for x in a if x in ("start", "status", "stop", "wait-ready", "wait-done")), "?"),
        "obs": lambda a: a[-1],
        "guard": lambda a: "guard",
        "verdict": lambda a: "verdict"}}[kind](argv)
path = os.path.join(d, f"{{kind}}.{{sub}}.q")
lines = open(path).read().splitlines() if os.path.exists(path) else []
if not lines:
    rc, out = 0, {{"ok": True}}
else:
    rc, out = json.loads(lines[0])
    if len(lines) > 1:
        open(path, "w").write("\n".join(lines[1:]) + "\n")
if kind == "verdict":
    o = argv[argv.index("--out") + 1]
    os.makedirs(o, exist_ok=True)
    if out is not None:
        json.dump(out, open(os.path.join(o, "youtube-leg-verdict.json"), "w"))
    print("youtube-leg verdict: fake")
elif kind == "guard":
    print("program-audio verdict=" + ("MEASUREMENT" if rc == 0 else "FOREIGN") + " rms=-20.0")
else:
    print(json.dumps(out))
sys.exit(rc)
'''


SSH = r'''#!{py}
import json, os, subprocess, sys
d = {d!r}
host, cmd = sys.argv[-2], sys.argv[-1]
with open(os.path.join(d, "calls.log"), "a") as f:
    secret = os.environ.get("FAKE_SECRET")  # the password must never reach ssh's argv
    f.write(json.dumps(["ssh", host, "leak" if secret and secret in " ".join(sys.argv) else "-"]) + "\n")
if os.path.exists(os.path.join(d, "ssh.down")):
    print(f"ssh: connect to host {{host}} port 22: No route to host", file=sys.stderr)
    sys.exit(255)
env = {{"HOME": {home!r}, "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"}}
r = subprocess.run(["bash", "-c", cmd], env=env, cwd={home!r})
sys.exit(r.returncode)
'''

REMOTE_PY = r'''#!{py}
import json, os, sys, time
d = {d!r}
argv = sys.argv[2:]  # [the tool's path, its arguments]
kind = "prime" if "--prime-ticks" in argv else "verdict"
import signal


def _term(*_):  # the real tool's handler kills its children on SIGTERM: record that it got the TERM
    open(os.path.join(d, "remote-term"), "a").write(kind + "\\n")
    sys.exit(143)


signal.signal(signal.SIGTERM, _term)
with open(os.path.join(d, "calls.log"), "a") as f:
    f.write(json.dumps([kind, sys.argv[1]] + argv) + "\n")
path = os.path.join(d, f"{{kind}}.{{kind}}.q")
lines = open(path).read().splitlines() if os.path.exists(path) else []
rc, out = (0, {{"ok": True}}) if not lines else json.loads(lines[0])
if len(lines) > 1:
    open(path, "w").write("\n".join(lines[1:]) + "\n")
if isinstance(out, dict) and out.get("sleep"):
    time.sleep(out["sleep"])
o = argv[argv.index("--out") + 1]
os.makedirs(o, exist_ok=True)
if kind == "prime":
    if rc == 0:
        open(os.path.join(o, "ticks-rec-1.tsv"), "w").write("# fake key\n0\t0.000\t10\tB\t10\t9\n")
        json.dump({{"frames": 1, "known": 1, "fallback": 0, "full": 1}},
                  open(os.path.join(o, "ticks-rec-1.tsv.reads.json"), "w"))
    else:
        print("youtube_leg_verdict: the early decode failed: fake", file=sys.stderr)
    sys.exit(rc)
if out is not None and "sleep" not in out:
    json.dump(out, open(os.path.join(o, "youtube-leg-verdict.json"), "w"))
open(os.path.join(o, "ticks-vod.tsv"), "w").write("# fake key\n0\t0.000\t10\tB\t10\t9\n")
open(os.path.join(o, "vod-AbCdEfGhIjK.mp4"), "w").write("a video")
print("youtube-leg verdict: fake")
sys.exit(rc)
'''

SSHPASS = r'''#!{py}
import json, os, shutil, sys
d = {d!r}
argv = sys.argv[1:]
pw = os.environ.get("SSHPASS")
src, dst = argv[-2], argv[-1]
with open(os.path.join(d, "calls.log"), "a") as f:
    f.write(json.dumps(["sshpass", src, "pw" if pw else "nopw", "leak" if pw and any(pw in a for a in argv) else "-"])
            + "\n")
fail = os.path.join(d, "pull.fail")
if os.path.exists(fail):
    rc = int(open(fail).read().strip() or 1)
    os.remove(fail)
    print("scp: Connection closed", file=sys.stderr)
    sys.exit(rc)  # scp exits 255 when the stream box cannot be reached
shutil.copyfile(os.path.join(d, "stream") + src.split(":", 1)[1], dst)
'''


class Fakes:
    def __init__(self, tmp_path):
        self.d = Path(tmp_path) / "fakes"
        self.d.mkdir(parents=True, exist_ok=True)
        self.env = dict(os.environ)
        self.marker = self.d / "youtube-leg-live.json"  # never the real dev1 marker
        self.env["YOUTUBE_LEG_MARKER"] = str(self.marker)
        for kind in ("session", "obs", "guard"):
            exe = self.d / kind
            exe.write_text(FAKE.format(py=sys.executable, kind=kind, d=str(self.d)))
            exe.chmod(0o755)
            self.env[f"YOUTUBE_LEG_{kind.upper()}_CMD"] = str(exe)
        # the measurement host: a fake dev2 home whose ~/.local/bin holds the tool + sshpass stubs
        self.home = self.d / "dev2-home"
        self.stream = self.d / "stream"
        bin_dir = self.home / ".local" / "bin"
        bin_dir.mkdir(parents=True, exist_ok=True)
        self.stream.mkdir(exist_ok=True)
        for name, text in (("python3", REMOTE_PY), ("sshpass", SSHPASS)):
            (bin_dir / name).write_text(text.format(py=sys.executable, d=str(self.d)))
            (bin_dir / name).chmod(0o755)
        ssh = self.d / "ssh"
        ssh.write_text(SSH.format(py=sys.executable, d=str(self.d), home=str(self.home)))
        ssh.chmod(0o755)
        self.env["YOUTUBE_LEG_SSH_CMD"] = str(ssh)
        self.env["YOUTUBE_LEG_MEASURE_HOST"] = "newlevel@dev2"

    def stream_file(self, path, data=b"a stream recording"):
        """A file on the fake stream box (what the fake pull copies)."""
        p = self.stream / path.lstrip("/")
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        return path

    def host_down(self):
        (self.d / "ssh.down").write_text("")

    def fail_one_pull(self, rc=1):
        (self.d / "pull.fail").write_text(str(rc))

    def host_stage_dirs(self):
        base = self.home / "youtube-leg-1404"
        return sorted(p.name for p in base.iterdir()) if base.exists() else []

    def script(self, kind, sub, rc, out):
        with open(self.d / f"{kind}.{sub}.q", "a") as f:
            f.write(json.dumps([rc, out]) + "\n")

    def calls(self):
        p = self.d / "calls.log"
        return [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []

    def names(self):
        """The calls as short names: session.start, obs.stream-start, guard, verdict."""
        out = []
        for c in self.calls():
            kind, argv = c[0], c[1:]
            if kind == "session":
                out.append("session." + next(x for x in argv if x in ("start", "status", "stop", "wait-ready", "wait-done")))
            elif kind == "obs":
                out.append("obs." + argv[-1])
            else:
                out.append(kind)
        return out
