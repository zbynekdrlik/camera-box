"""Issue 1404 -- fakes for the full-path E2E's YouTube stage tests (not a test module).

`Fakes(tmp_path)` writes four executables (session, obs, guard, verdict) that log every call to
calls.log and answer from per-kind scripts: `fakes.script("session", "start", rc, {json})` queues one
answer for that subcommand (the last queued answer repeats). The fake verdict tool writes the queued
JSON as <--out>/youtube-leg-verdict.json and exits with the queued rc. `fakes.env` points the
orchestrator's YOUTUBE_LEG_*_CMD seams at them.
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


class Fakes:
    def __init__(self, tmp_path):
        self.d = Path(tmp_path) / "fakes"
        self.d.mkdir(parents=True, exist_ok=True)
        self.env = dict(os.environ)
        for kind in ("session", "obs", "guard", "verdict"):
            exe = self.d / kind
            exe.write_text(FAKE.format(py=sys.executable, kind=kind, d=str(self.d)))
            exe.chmod(0o755)
            self.env[f"YOUTUBE_LEG_{kind.upper()}_CMD"] = str(exe)

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
