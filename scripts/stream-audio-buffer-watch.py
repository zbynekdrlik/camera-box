#!/usr/bin/env python3
"""dev1-side stream-OBS audio-buffering watcher (#786) — closes the .lnk hole.

The launch-obs-genlock.sh (3b) gate only guards launches the AGENT drives; an
operator double-clicking the OBS shortcut gets no gate. This timer (systemd
--user, every 2 min) reads the NEWEST stream-OBS log over read-only SMB and
fires ONE Discord alarm per log file when the session's audio-buffering peak
exceeds THRESHOLD_MS (box standard 64/85 ms; a bad ASIO launch draw ratchets
to 960 ms max, sticky until OBS restart -> whole-session A/V off by ~0.9 s,
live incident 2026-07-15). Detection-only: it NEVER touches OBS.

Self-test (no SMB, no send): stream-audio-buffer-watch.py --selftest <logfile>
"""
import os
import re
import subprocess
import sys

SMB_UNC = "//10.77.9.204/C$"
SMB_AUTH = "newlevel%newlevel"
LOGS_DIR = r"Users\newlevel\AppData\Roaming\obs-studio\logs"
THRESHOLD_MS = 100  # box standard 64/85 ms + headroom — same bound as launch-obs-genlock.sh (3b)
STATE_DIR = os.path.expanduser("~/.local/state/stream-audio-buffer-watch")
STATE = os.path.join(STATE_DIR, "alerted")  # lines: "<logname>|<peak>"
NOTIFY = [sys.executable, os.path.expanduser("~/devel/airuleset/airuleset.py"), "notify"]
ADD_RE = re.compile(r"total audio buffering is now (\d+) milliseconds")


def peak_of(text: str) -> tuple[int, bool]:
    peak = 0
    for m in ADD_RE.finditer(text):
        v = int(m.group(1))
        if v > peak:
            peak = v
    return peak, ("Max audio buffering reached" in text)


def compose(logname: str, peak: int, maxed: bool) -> str:
    tag = " (STROP 960 = max račňa)" if maxed else ""
    return (f"🚨 stream OBS: audio buffer {peak} ms{tag}, norma je 64 ms — zvuk ide o ~{peak} ms "
            f"neskôr než má, A/V sync je MIMO. Zlý ASIO žreb pri štarte OBS (session log {logname}). "
            f"Fix: reštartni stream OBS a over, že buffer je späť na 64 ms (#786).")


def smb(cmd: str, timeout: int = 25) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["smbclient", SMB_UNC, "-U", SMB_AUTH, "-c", cmd],
        capture_output=True, text=True, timeout=timeout)


def newest_log() -> str | None:
    out = smb(f"ls {LOGS_DIR}\\*.txt")
    if out.returncode != 0:
        print(f"watch: smb ls failed rc={out.returncode}: {out.stderr.strip()[:120]} — retry next tick")
        return None
    names = re.findall(r"^\s+(\d{4}-\d{2}-\d{2} \d{2}-\d{2}-\d{2}\.txt)\b", out.stdout, re.M)
    return max(names) if names else None  # name IS the launch timestamp — lexical max = newest session


def fetch_log(name: str) -> str | None:
    local = os.path.join(STATE_DIR, "current.txt")
    out = smb(f'get "{LOGS_DIR}\\{name}" {local}', timeout=60)
    if out.returncode != 0:
        print(f"watch: smb get failed rc={out.returncode}: {out.stderr.strip()[:120]} — retry next tick")
        return None
    with open(local, encoding="utf-8", errors="replace") as f:
        return f.read()


def main() -> int:
    os.makedirs(STATE_DIR, exist_ok=True)
    if len(sys.argv) == 3 and sys.argv[1] == "--selftest":
        with open(sys.argv[2], encoding="utf-8", errors="replace") as f:
            peak, maxed = peak_of(f.read())
        if peak > THRESHOLD_MS or maxed:
            print(f"SELFTEST WOULD ALERT: {compose(os.path.basename(sys.argv[2]), peak, maxed)}")
        else:
            print(f"SELFTEST CLEAN: peak={peak} ms maxed={maxed} (threshold {THRESHOLD_MS})")
        return 0

    name = newest_log()
    if not name:
        return 0
    text = fetch_log(name)
    if text is None:
        return 0
    peak, maxed = peak_of(text)
    if peak <= THRESHOLD_MS and not maxed:
        print(f"watch: {name} clean (peak {peak} ms)")
        return 0
    key = f"{name}|{peak}"
    if not os.path.exists(STATE):
        open(STATE, "w").close()
    with open(STATE) as f:
        alerted = set(line.strip() for line in f if line.strip())
    if key in alerted:
        print(f"watch: {name} bad (peak {peak} ms) — already alerted")
        return 0
    out = subprocess.run(NOTIFY + ["--body", compose(name, peak, maxed)],
                         capture_output=True, text=True, timeout=30)
    if out.returncode != 0:
        print(f"watch: notify failed rc={out.returncode}: {out.stderr.strip()[:120]} — retry next tick")
        return 0
    with open(STATE, "a") as f:
        f.write(key + "\n")
    print(f"watch: ALERT sent for {name} (peak {peak} ms, maxed={maxed})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
