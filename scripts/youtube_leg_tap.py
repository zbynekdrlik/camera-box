#!/usr/bin/env python3
"""Issue 1387 -- the restreamer-INPUT measurement point of the full-path E2E's YouTube stage.

The YouTube VOD shows A/V steps of 3-18 ms at its 5 s segment starts while the stream OBS recording
has none. Between them sit restreamer (on the stream box: RTMP inpoint :1234, 1 s chunks to S3) and
its per-session delivery VPS (`rs_delivery`, its own RTMP pusher re-timing every 2 s chunk) and
YouTube. This tool records what restreamer RECEIVES: one more RTMP player on restreamer's own
inpoint (the stream-headphone monitor plays the same URL), `ffmpeg -c copy` into a Matroska file, so
the capture holds the FLV tags restreamer got, timestamps kept (`-copyts`), nothing transcoded. The
YouTube stage then judges that file with the same tool as the VOD (scripts/youtube_leg_e2e.py, the
`points` of its block): REPORT-ONLY, it never changes the YouTube verdict.

restreamer's OUTPUT lives only on the delivery VPS (created and deleted per session, outside
camera-box), so it cannot be tapped without a change in restreamer: OUTPUT_UNTAPPED names that.

A measurement must never cost the broadcast a frame (owner rule 1.11.2026): the capture is a copy
only, runs in rig-background.slice through /usr/local/sbin/obs-box-background-run when the box
allows it (nice 19, ionice idle), always under `chrt --idle 0`, pinned to the CPUs the OBS mask
(/etc/*-isolated-cpus.conf) leaves free, never CPU 15 (RESERVED_CPUS). No free CPU = no capture.

  youtube_leg_tap.py --host H --user U --key K start --url rtmp://127.0.0.1:1234/live/<key>
  youtube_leg_tap.py --host H --user U --key K stop [--grace S]
  youtube_leg_tap.py --host H --user U --key K purge

stdout = ONE JSON object (`ok` true / false + `reason`); exit 0 OK, 2 refused / failed. The capture
lives in `~/youtube-leg-tap-1387/<key>/inpoint.mkv` on the stream box; a dir older than a day (a
killed run) is swept at the next start. The ssh password comes from $AV_GATE_SSH_PASSWORD (sshpass
-e), never an argument. Test seam: YOUTUBE_LEG_TAP_SSH_CMD (an executable taking `<user@host>
<remote bash>`).
"""
import argparse
import json
import os
import re
import shlex
import subprocess
import sys

EXIT_OK, EXIT_FAIL = 0, 2
BASE = "youtube-leg-tap-1387"
CAPTURE = "inpoint.mkv"
RESERVED_CPUS = (15,)  # the owner's rule for a measurement on a broadcast box: never on CPU 15
BACKGROUND_RUN = "/usr/local/sbin/obs-box-background-run"
SSH_OPTS = ["-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR",
            "-o", "ConnectTimeout=10"]
STEP_TIMEOUT_S = 60
STOP_GRACE_S = 15   # ffmpeg ends by itself when the publisher stops; this long before it is signalled
STALE_MIN = 1440
RW_TIMEOUT_US = 10_000_000  # no data for 10 s = the capture ends (a vanished publisher never hangs it)
LOCAL_INPOINTS = re.compile(r"^rtmp://(127\.0\.0\.1|localhost):1234/[A-Za-z0-9_.-]+/?$")
KEY_OK = re.compile(r"^[A-Za-z0-9_.-]{1,64}$")
OUTPUT_UNTAPPED = ("restreamer's output exists only on its per-session delivery VPS (rs_delivery pushes the "
                   "chunks from S3 to YouTube); camera-box cannot record it without a change in restreamer")
MARK = "TAP-RESULT "


# ------------------------------------------------------------------ pure builders

def cpulist_expand(text):
    """'0-1,12-14' -> {0, 1, 12, 13, 14}; ValueError on anything but a kernel cpu list."""
    text = (text or "").strip()
    if not re.fullmatch(r"[0123456789]+(-[0123456789]+)?(,[0123456789]+(-[0123456789]+)?)*", text):
        raise ValueError(f"not a cpu list: {text!r}")
    out = set()
    for item in text.split(","):
        a, _, b = item.partition("-")
        lo, hi = int(a), int(b or a)
        if lo > hi:
            raise ValueError(f"not a cpu list: {text!r}")
        out.update(range(lo, hi + 1))
    return out


def cpulist_compress(cpus):
    """{0, 1, 12, 13} -> '0-1,12-13'."""
    out, run = [], []
    for c in sorted(cpus):
        if run and c == run[-1] + 1:
            run.append(c)
            continue
        if run:
            out.append(f"{run[0]}-{run[-1]}" if len(run) > 1 else str(run[0]))
        run = [c]
    if run:
        out.append(f"{run[0]}-{run[-1]}" if len(run) > 1 else str(run[0]))
    return ",".join(out)


def tap_cpus(online, obs_masks, reserved=RESERVED_CPUS):
    """The capture's CPUs: online minus every OBS mask minus the reserved CPUs, as a cpu list; None
    when nothing is left or a list is unreadable (no OBS mask at all = None too: never every CPU)."""
    try:
        cpus = cpulist_expand(online)
        masks = [cpulist_expand(m) for m in obs_masks]
    except ValueError:
        return None
    if not masks:
        return None
    for m in masks:
        cpus -= m
    cpus -= set(reserved)
    return cpulist_compress(cpus) if cpus else None


def local_inpoint_url(server, key):
    """restreamer's local inpoint URL for the stream OBS's service settings, or None (any other server
    is not restreamer on this box: never recorded)."""
    if not isinstance(server, str) or not LOCAL_INPOINTS.match(server.strip()):
        return None
    if not isinstance(key, str) or not KEY_OK.match(key):
        return None
    return server.strip().rstrip("/") + "/" + key


def capture_argv(url, out):
    """The capture: a copy of every stream restreamer received, its timestamps kept."""
    return ["ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "warning", "-rw_timeout", str(RW_TIMEOUT_US),
            "-i", url, "-map", "0", "-c", "copy", "-copyts", "-f", "matroska", out]


def priority_prefix(cpus, helper):
    """Idle priority off the OBS CPUs: rig-background.slice through the box's helper when sudo allows
    it, else nice 19 + ionice idle; SCHED_IDLE and the CPU pin either way."""
    pin = ["chrt", "--idle", "0", "taskset", "-c", cpus]
    if helper:
        return ["sudo", "-n", BACKGROUND_RUN] + pin
    return ["nice", "-n", "19", "ionice", "-c", "3"] + pin


def _dir_expr(key):
    if not KEY_OK.match(key or ""):
        raise ValueError(f"bad stage key {key!r}")
    return f'"$HOME"/{BASE}/{key}'


def probe_script():
    """The stream box's CPU facts and whether the background helper may be run."""
    # $YOUTUBE_LEG_TAP_ROOT is a test seam (a fake root); unset on a real box
    return ("set -u\nR=\"${YOUTUBE_LEG_TAP_ROOT:-}\"\n"
            "printf 'online=%s\\n' \"$(cat \"$R/sys/devices/system/cpu/online\" 2>/dev/null)\"\n"
            "for f in \"$R\"/etc/*-isolated-cpus.conf; do [ -r \"$f\" ] && printf 'obs=%s\\n' \"$(head -n 1 \"$f\")\"; done\n"
            f"if [ -x \"$R{BACKGROUND_RUN}\" ] && sudo -n -l \"$R{BACKGROUND_RUN}\" >/dev/null 2>&1; then echo helper=1; "
            "else echo helper=0; fi\n"
            "command -v ffmpeg >/dev/null 2>&1 && echo ffmpeg=1 || echo ffmpeg=0\n")


def parse_probe(text):
    facts = {"online": "", "obs": [], "helper": False, "ffmpeg": False}
    for line in (text or "").splitlines():
        k, _, v = line.partition("=")
        if k == "online":
            facts["online"] = v.strip()
        elif k == "obs" and v.strip():
            facts["obs"].append(v.strip())
        elif k in ("helper", "ffmpeg"):
            facts[k] = v.strip() == "1"
    return facts


def start_script(key, argv):
    """Sweep a day-old leftover, start the capture detached in its own session, prove it still runs."""
    d = _dir_expr(key)
    cmd = " ".join(shlex.quote(a) for a in argv)
    return (f"set -euo pipefail\nD={d}\nmkdir -p \"$(dirname \"$D\")\"\n"
            f"find \"$(dirname \"$D\")\" -mindepth 1 -maxdepth 1 -type d -mmin +{STALE_MIN} -exec rm -rf -- {{}} + || true\n"
            "rm -rf -- \"$D\"\nmkdir -p \"$D\"\ncd \"$D\"\n"
            f"setsid nohup {cmd} > \"$D/ffmpeg.log\" 2>&1 < /dev/null &\n"
            "echo \"$!\" > \"$D/tap.pid\"\nsleep 2\n"
            "if [ -d \"/proc/$(cat \"$D/tap.pid\")\" ]; then "
            f"printf '{MARK}%s\\n' \"{{\\\"running\\\":true,\\\"path\\\":\\\"$D/{CAPTURE}\\\"}}\"; "
            "else tail -c 400 \"$D/ffmpeg.log\" >&2 || true; rm -f \"$D/tap.pid\"; "
            f"printf '{MARK}%s\\n' '{{\"running\":false}}'; fi\n")


def stop_script(key, grace=STOP_GRACE_S):
    """Wait for the capture to end by itself (the publisher stopped), else INT then KILL its group."""
    d = _dir_expr(key)
    g = int(grace)
    return (f"set -uo pipefail\nD={d}\nhow=absent\n"
            "if [ -f \"$D/tap.pid\" ]; then p=\"$(cat \"$D/tap.pid\")\"; how=ended\n"
            f"  for i in $(seq 1 {g}); do [ -d \"/proc/$p\" ] || break; sleep 1; done\n"
            "  if [ -d \"/proc/$p\" ]; then how=signalled; kill -INT -- \"-$p\" 2>/dev/null || true\n"
            "    for i in $(seq 1 10); do [ -d \"/proc/$p\" ] || break; sleep 1; done\n"
            "    if [ -d \"/proc/$p\" ]; then how=killed; kill -KILL -- \"-$p\" 2>/dev/null || true; fi\n"
            "  fi\n  rm -f \"$D/tap.pid\"\nfi\n"
            f"b=\"$(stat -c %s \"$D/{CAPTURE}\" 2>/dev/null || echo 0)\"\n"
            "log=\"$(tail -c 300 \"$D/ffmpeg.log\" 2>/dev/null | tr -d '\\\\\"' | tr '\\n\\r\\t' '   ' || true)\"\n"
            f"printf '{MARK}{{\"how\":\"%s\",\"bytes\":%s,\"path\":\"%s\",\"log\":\"%s\"}}\\n' "
            f"\"$how\" \"$b\" \"$D/{CAPTURE}\" \"$log\"\n")


def purge_script(key):
    d = _dir_expr(key)
    return (f"set -uo pipefail\nD={d}\n"
            "if [ -f \"$D/tap.pid\" ]; then kill -KILL -- \"-$(cat \"$D/tap.pid\")\" 2>/dev/null || true; fi\n"
            f"rm -rf -- \"$D\"\nprintf '{MARK}%s\\n' '{{\"purged\":true}}'\n")


def parse_result(text):
    """The JSON object after the last TAP-RESULT marker, or None."""
    for line in reversed((text or "").splitlines()):
        if line.startswith(MARK):
            try:
                v = json.loads(line[len(MARK):])
            except ValueError:
                return None
            return v if isinstance(v, dict) else None
    return None


# ------------------------------------------------------------------ the stream box

class Refused(Exception):
    pass


class Box:
    def __init__(self, host, user):
        self.host, self.user = host, user
        override = os.environ.get("YOUTUBE_LEG_TAP_SSH_CMD")
        self.ssh = [override] if override else ["sshpass", "-e", "ssh", "-n", *SSH_OPTS]

    def run(self, script, what, timeout=STEP_TIMEOUT_S):
        env = dict(os.environ, SSHPASS=os.environ.get("AV_GATE_SSH_PASSWORD", ""))
        cmd = self.ssh + [f"{self.user}@{self.host}", "bash -c " + shlex.quote(script)]
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=env, stdin=subprocess.DEVNULL)
        except subprocess.TimeoutExpired:
            raise Refused(f"{what} on {self.host} ran over {timeout} s") from None
        except OSError as e:
            raise Refused(f"cannot run ssh to {self.host}: {e}") from None
        if r.returncode == 255:
            raise Refused(f"{self.host} is unreachable ({what}): {(r.stderr or '').strip()[-200:]}")
        return r


def do_start(box, key, url):
    if not url:
        raise Refused("no restreamer inpoint URL (the stream OBS does not publish to restreamer on this box)")
    r = box.run(probe_script(), "the CPU probe")
    facts = parse_probe(r.stdout)
    if not facts["ffmpeg"]:
        raise Refused(f"ffmpeg is not installed on {box.host}")
    cpus = tap_cpus(facts["online"], facts["obs"])
    if not cpus:
        raise Refused(f"no CPU outside the OBS mask {facts['obs'] or '(none read)'} and CPU 15 on {box.host} "
                      f"(online {facts['online'] or '?'}): never captured on the OBS cores")
    prefix = priority_prefix(cpus, facts["helper"])
    argv = prefix + capture_argv(url, CAPTURE)
    r = box.run(start_script(key, argv), "the capture start")
    res = parse_result(r.stdout)
    if not res or not res.get("running"):
        raise Refused(f"the capture of {url} did not keep running: {(r.stderr or r.stdout).strip()[-300:]}")
    return {"started": True, "path": res.get("path"), "url": url, "cpus": cpus,
            "priority": "rig-background.slice + SCHED_IDLE" if facts["helper"] else "nice 19 + ionice idle + SCHED_IDLE"}


def do_stop(box, key, grace=STOP_GRACE_S):
    r = box.run(stop_script(key, grace), "the capture stop", timeout=grace + 40)
    res = parse_result(r.stdout)
    if res is None:
        raise Refused(f"the capture stop gave no result: {(r.stderr or r.stdout).strip()[-300:]}")
    return {"stopped": True, "how": res.get("how"), "bytes": res.get("bytes"), "path": res.get("path"),
            "ffmpeg_log": res.get("log") or ""}


def do_purge(box, key):
    r = box.run(purge_script(key), "the capture purge")
    if parse_result(r.stdout) is None:
        raise Refused(f"the capture purge gave no result: {(r.stderr or r.stdout).strip()[-300:]}")
    return {"purged": True}


def main(argv=None):
    ap = argparse.ArgumentParser(description="issue 1387: the restreamer-input capture on the stream box")
    ap.add_argument("--host", required=True)
    ap.add_argument("--user", default="newlevel")
    ap.add_argument("--key", required=True, help="the stage key (the dev1 run dir's name)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("start")
    p.add_argument("--url", default="")
    p = sub.add_parser("stop")
    p.add_argument("--grace", type=float, default=STOP_GRACE_S)
    sub.add_parser("purge")
    a = ap.parse_args(argv)
    box = Box(a.host, a.user)
    try:
        if a.cmd == "start":
            out = do_start(box, a.key, a.url)
        elif a.cmd == "stop":
            out = do_stop(box, a.key, a.grace)
        else:
            out = do_purge(box, a.key)
    except (Refused, ValueError) as e:
        print(json.dumps({"ok": False, "reason": str(e)}))
        return EXIT_FAIL
    print(json.dumps(dict(out, ok=True)))
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
