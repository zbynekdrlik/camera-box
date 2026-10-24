#!/usr/bin/env python3
"""Issue 1404 (main design 6387919956, part c) -- the YouTube-leg measurement on the measurement host.

The full-path E2E's YouTube stage measures on dev2 (8 cores; the owner's no-rig-data-on-dev1 rule),
never on dev1: dev1 only orchestrates (scripts/youtube_leg_e2e.py) and folds the verdict JSON. On the
host, under `~/youtube-leg-1404/<stage key>/`:

  code/                    the verdict tool's modules (shipped from the E2E's own checkout every run)
  in/                      the cam2 marker log + the recording-verdict probe binary
  stream-recording.<ext>   pulled from the stream box STRAIGHT to the host (sshpass -e scp; the
                           stream box password arrives on the ssh session's stdin, never in argv)
  verdict/                 the tool's out dir: the tick maps, the VOD, youtube-leg-verdict.json
  <step>.pid               the process group of a running long step (`timeout` leads its own group)

Access: plain key-auth ssh, the access the dev2 avsync measurer already uses (`newlevel@dev2`,
scripts/avsync-dev2-install.sh); no new credential. Every remote step is bounded twice: `timeout`
on the host (it kills its whole process group: the decode pool, yt-dlp's ffmpeg, the probe) and
`youtube_leg_proc.run_bounded` on dev1 (it kills the local ssh's group). ssh's own exit 255 (the
host unreachable, the key refused) raises HostError(unreachable=True): the stage is then UNKNOWN by
name, never measured on dev1 instead. `cleanup()` kills every recorded remote group and deletes the
stage dir, the videos with it; `stage_code()` also sweeps stage dirs a killed run left (over a day).

Test seam: YOUTUBE_LEG_SSH_CMD (an executable taking ssh's `<host> <remote command>` arguments).
"""
import io
import os
import re
import shlex
import subprocess
import sys
import tarfile

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from youtube_leg_proc import run_bounded  # noqa: E402

DEFAULT_HOST = "newlevel@dev2"
# dev2 also runs the SyncNet measurer (one run at a time, flock'd, ~4 of its 8 cores busy): the decode
# pool takes 4 workers at nice 10, so the measurer keeps its share and the decode the rest
DEFAULT_WORKERS = 4
NICE = 10
BASE = "youtube-leg-1404"
TOOL_FILES = ("youtube_leg_verdict.py", "youtube_leg_ticks.py", "youtube_leg_timeline.py", "youtube_leg_audio.py",
              "youtube_leg_proc.py")
SSH_OPTS = ("-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15",
            "-o", "ServerAliveCountMax=4")
# the stream box: scripts/lib/stream-box.sh's transport options (a stale known_hosts key with
# StrictHostKeyChecking=no would disable password authentication)
STREAM_SSH_OPTS = "-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10"
REMOTE_PATH = '"$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"'  # yt-dlp lives in ~/.local/bin (pipx)
CHECK_TIMEOUT_S, STAGE_TIMEOUT_S, FETCH_TIMEOUT_S, CLEAN_TIMEOUT_S = 30, 120, 120, 60
PULL_TIMEOUT_S = 900  # the bound the gate used for the recording's download to dev1
KILL_AFTER_S = 30     # `timeout -k`: SIGKILL the group this long after its SIGTERM
LOCAL_SLACK_S = 60    # the local bound = the remote bound + this (ssh + the kill-after)
STALE_MIN = 1440      # a stage dir older than a day is a killed run's leftover
TIMED_OUT = (124, 137)  # `timeout`'s exit after its SIGTERM / its SIGKILL
FETCHED = re.compile(r"^(youtube-leg-verdict\.json|ticks-[A-Za-z0-9_.-]+\.tsv|[A-Za-z0-9_.-]+\.reads\.json)$")


class HostError(RuntimeError):
    """A step on the measurement host failed; `unreachable` = ssh itself failed (exit 255)."""

    def __init__(self, msg, unreachable=False):
        super().__init__(msg)
        self.unreachable = unreachable


def stage_key(stage_dir):
    """The host-side name of a stage: the dev1 run dir's name (`recording-e2e-<id>`), sanitized."""
    name = os.path.basename(os.path.dirname(os.path.abspath(stage_dir)))
    return re.sub(r"[^A-Za-z0-9._-]", "_", name).lstrip(".") or "stage"


def _tail(b):
    text = b.decode("utf-8", "replace") if isinstance(b, bytes) else (b or "")
    return " ".join(text.split())[-400:]


def _text(r):
    return ((r.stdout or b"") + (r.stderr or b"")).decode("utf-8", "replace")


class Host:
    def __init__(self, host=DEFAULT_HOST, key="stage", workers=DEFAULT_WORKERS):
        self.host = host
        self.key = key
        self.workers = max(1, int(workers))
        override = os.environ.get("YOUTUBE_LEG_SSH_CMD")
        self.ssh = [override] if override else ["ssh", *SSH_OPTS]
        self.root = None  # the stage dir's absolute path on the host (check() reads the host's $HOME)

    def describe(self):
        return f"{self.host}:{self.root or '~/' + BASE + '/' + self.key}"

    def path(self, *parts):
        """An absolute path inside the stage dir (check() must have run)."""
        if self.root is None:
            raise HostError("the measurement host was never checked (no stage dir)")
        return "/".join((self.root,) + parts)

    def run(self, script, timeout, what, input=None):
        """`script` (bash, strict mode, $R = the stage dir) on the host; the CompletedProcess (bytes).
        HostError: ssh failed (unreachable) or the local bound ran out."""
        root = f"R={shlex.quote(self.root)}; " if self.root else ""
        body = f"set -euo pipefail; {root}export PATH={REMOTE_PATH}; {script}"
        cmd = self.ssh + [self.host, "bash -c " + shlex.quote(body)]
        try:
            r = run_bounded(cmd, timeout, input=input)
        except subprocess.TimeoutExpired:
            raise HostError(f"{what} on the measurement host {self.host} ran over {timeout:.0f} s") from None
        except OSError as e:
            raise HostError(f"cannot run ssh to the measurement host {self.host}: {e}", unreachable=True) from None
        if r.returncode == 255:
            raise HostError(f"the measurement host {self.host} is unreachable ({what}): {_tail(r.stderr)}",
                            unreachable=True)
        return r

    @staticmethod
    def _bounded(step, cmd, timeout, then=""):
        """A long remote step: `timeout` leads its own process group, its pid is recorded for
        cleanup(), the script's exit code is the step's (124/137 = the bound ran out)."""
        return (f'timeout -k {KILL_AFTER_S} {int(timeout)} nice -n {NICE} {cmd} & echo "$!" > "$R/{step}.pid"; '
                f'rc=0; wait "$!" || rc=$?; rm -f "$R/{step}.pid"; {then}exit "$rc"')

    def check(self):
        """The host answers a plain command; reads its $HOME (the stage dir's root)."""
        r = self.run('printf "%s" "$HOME"', CHECK_TIMEOUT_S, "the reachability check")
        home = (r.stdout or b"").decode("utf-8", "replace").strip()
        if r.returncode != 0 or not home.startswith("/"):
            raise HostError(f"the measurement host {self.host} refused a plain command: {_tail(r.stderr)}")
        self.root = f"{home}/{BASE}/{self.key}"

    def _put(self, files, sub, what):
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w") as tar:
            for name, src in files.items():
                tar.add(src, arcname=name, recursive=False)
        r = self.run(f'mkdir -p "$R/{sub}" && tar -x -C "$R/{sub}" -f -', STAGE_TIMEOUT_S, what, input=buf.getvalue())
        if r.returncode != 0:
            raise HostError(f"{what} to {self.describe()} failed: {_tail(r.stderr)}")

    def stage_code(self):
        """Ship the verdict tool's modules; sweep a day-old leftover of a killed run first."""
        self.run(f'mkdir -p "$(dirname "$R")" && find "$(dirname "$R")" -mindepth 1 -maxdepth 1 -type d '
                 f'-mmin +{STALE_MIN} -exec rm -rf -- {{}} + || true', STAGE_TIMEOUT_S, "the stale stage sweep")
        self._put({f: os.path.join(_HERE, f) for f in TOOL_FILES}, "code", "shipping the verdict tool")

    def put_inputs(self, markers, probe_bin):
        self._put({"markers.csv": markers, "recording-verdict": probe_bin}, "in",
                  "shipping the marker log and the probe binary")

    def has(self, name):
        r = self.run(f'test -s "$R/{name}"', CHECK_TIMEOUT_S, f"looking for {name}")
        return r.returncode == 0

    def pull(self, user, password, stream_host, stream_path, name, timeout=PULL_TIMEOUT_S):
        """The stream recording, from the stream box straight to the host (never via dev1)."""
        src = shlex.quote(f"{user}@{stream_host}:{stream_path}")
        script = ('IFS= read -r SSHPASS; export SSHPASS; mkdir -p "$R"; rm -f "$R/' + name + '.part"; '
                  + self._bounded("pull", f'sshpass -e scp {STREAM_SSH_OPTS} {src} "$R/{name}.part"', timeout,
                                  then=f'[ "$rc" = 0 ] && mv -f "$R/{name}.part" "$R/{name}"; '))
        r = self.run(script, timeout + LOCAL_SLACK_S, "the recording pull", input=(password + "\n").encode())
        if r.returncode != 0:
            how = f"ran over {timeout:.0f} s" if r.returncode in TIMED_OUT else f"failed (exit {r.returncode})"
            raise HostError(f"the stream recording pull {stream_host}:{stream_path} -> {self.describe()} {how}: "
                            f"{_tail(r.stderr)}")

    def _tool(self, step, args, timeout, what):
        cmd = "python3 " + " ".join(shlex.quote(a) for a in [self.path("code", "youtube_leg_verdict.py"), *args,
                                                              "--workers", str(self.workers)])
        r = self.run(self._bounded(step, cmd, timeout), timeout + LOCAL_SLACK_S, what)
        return r.returncode, _text(r)

    def prime(self, recording, timeout):
        """The early decode: the recording's tick map into the tool's cache (`--prime-ticks`).
        (rc, output); rc in TIMED_OUT = the bound ran out."""
        return self._tool("prime", ["--prime-ticks", recording, "--out", self.path("verdict")], timeout,
                          "the early decode")

    def run_tool(self, args, timeout):
        """The verdict tool on the host: (rc, output); rc in TIMED_OUT = the bound ran out."""
        return self._tool("verdict", args, timeout, "the verdict tool")

    def fetch(self, local_dir):
        """The verdict JSON + the tick maps (+ their read counters) back to dev1; the names fetched."""
        r = self.run('[ -d "$R/verdict" ] || exit 0; cd "$R/verdict"; find . -maxdepth 1 -type f \\( -name '
                     '"youtube-leg-verdict.json" -o -name "ticks-*.tsv" -o -name "*.reads.json" \\) -printf "%f\\0" '
                     '| tar -c -f - --null -T -', FETCH_TIMEOUT_S, "fetching the verdict")
        if r.returncode != 0:
            raise HostError(f"fetching the verdict from {self.describe()} failed: {_tail(r.stderr)}")
        os.makedirs(local_dir, exist_ok=True)
        got = []
        if r.stdout:
            with tarfile.open(fileobj=io.BytesIO(r.stdout), mode="r:") as tar:
                for m in tar.getmembers():  # only plain files with the names asked for, never a path
                    if m.isfile() and FETCHED.match(m.name):
                        tmp = os.path.join(local_dir, m.name + ".tmp")
                        with open(tmp, "wb") as f:
                            f.write(tar.extractfile(m).read())
                        os.replace(tmp, os.path.join(local_dir, m.name))
                        got.append(m.name)
        return sorted(got)

    def cleanup(self):
        """Kill every recorded remote step and delete the stage dir (the videos with it)."""
        if self.root is None:
            self.check()
        r = self.run('for p in "$R"/*.pid; do [ -f "$p" ] || continue; kill -KILL -- "-$(cat "$p")" 2>/dev/null '
                     '|| true; done; rm -rf -- "$R"', CLEAN_TIMEOUT_S, "the stage cleanup")
        if r.returncode != 0:
            raise HostError(f"the stage cleanup on {self.describe()} failed: {_tail(r.stderr)}")
