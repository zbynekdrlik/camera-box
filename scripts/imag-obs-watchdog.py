#!/usr/bin/env python3
# airuleset:script-ok watchdog must survive every per-item failure and keep polling; each swallow is logged
"""imag OBS render watchdog (#756) — ALARM-ONLY edition (user decision 2026-07-14
"len alarm na discord!!!" + "nemas co preventivne rebootovat imag nb, je to na nas").

THIS WATCHDOG NEVER REBOOTS THE BOX AND NEVER STOPS OUTPUTS. All reboot
decisions belong to the crew. It only: detects, snapshots, alarms, and (for a
dead OBS process only) relaunches OBS.

Poll obs-websocket GetStats every 5 s (RENDER loop stats, never encoder).
Trigger after 4 CONSECUTIVE bad samples (20 s hysteresis):
  bad = activeFps < 45 OR averageFrameRenderTime > 25 ms (renderTotal>3000
  guards fresh restarts; WS dark while the obs PID is alive counts bad too).

Tier (a) — OBS process DEAD (2 consecutive polls):
  relaunch OBS as newlevel + re-seed scenes/projectors (the #522 self-heal
  script) + write an INFO alarm record. No reboot — restarting a crashed
  process is recovery, not a reboot.
Tier (b) — process alive + render wedged:
  bounded <=60 s forensic snapshot (timed `nvidia-smi -q -d PERFORMANCE`
  [GSP-RPC discriminator], topline, gdb thread apply all bt, per-thread
  kernel stacks, lspci LnkSta, dmesg tail) -> write an ALARM record -> WAIT
  for the human. NO reboot. NO StopRecord/StopStream. One alarm per wedge
  episode; re-arms after recovery (fps>=58 x3) and writes a recovery INFO
  record.

Alarm records: one JSON line appended to /home/newlevel/imag-watchdog-alarms.jsonl
  {"id": "...", "type": "tier-b"|"tier-a"|"recovery", ...}
A dev1-side relay (imag-alarm-relay.timer) reads this file over SSH and fires
the Discord ping — the box itself has no webhook.

Test hook: `touch /home/newlevel/imag-watchdog-test-trigger` forces one tier-b
flow (file deleted BEFORE acting; cannot loop). Events log:
/home/newlevel/imag-watchdog-events.log. Archives: /home/newlevel/wedge-archive/<ts>/.

DEPLOY NOTE: this script was written + iterated directly on imag-nb across
several earlier sessions and had NO git-tracked source anywhere until this
commit (#756, 2026-07-15) — this file is now the tracked source of truth.
It is deployed manually to /usr/local/sbin/imag-obs-watchdog.py (root:root,
0755) with a systemd unit `imag-obs-watchdog.service`
(ExecStart=/usr/bin/python3 /usr/local/sbin/imag-obs-watchdog.py,
Restart=always). Wiring this into scripts/setup-imag.sh's own provisioning
(so a fresh imag box gets it automatically, and re-deploys pick up a fixed
script the same way the genlock hot-swap does) is tracked separately — see
the issue this commit's PR references — this file's existence here is the
"stop losing the source" fix, not yet the "auto-provisioned" fix.

#756 FIX (2026-07-15, live wedge investigation): relaunch_obs() used to launch a
fresh /usr/bin/obs WITHOUT first clearing OBS's own per-run .sentinel marker
files (~/.config/obs-studio/.sentinel/*, one empty file per run, deleted on a
CLEAN exit only). A tier-a relaunch follows a DEAD obs process -- by definition
never a clean exit -- so the stale sentinel from the crashed run was always
still present, and OBS's own crash-recovery check ("Crash or unclean shutdown
detected") hung the fresh process at that exact log line with near-idle CPU
and no further progress (confirmed live: a hot-swap's SIGKILL -> the
watchdog's own relaunch_obs() -> hung at the SAME "Crash or unclean shutdown
detected" stall the Windows genlock runbook already documents clearing
sentinels for -- .claude/skills/genlock's "clear sentinels -> Start-Process"
step never had a Linux/watchdog equivalent until now). Fix: clear
~/.config/obs-studio/.sentinel/* immediately before every relaunch, mirroring
the Windows launch-obs-genlock.sh wrapper's own sentinel-clear step. Live-
verified: manually reproduced the hang (kill -9 obs -> watchdog auto-relaunch
attempted, hung at "Crash or unclean shutdown detected", WS never came up
within 90s -> tier-a alarm fired "ws_up: false"), then patched + restarted
the service; the fix (this file) is now deployed and the box is healthy
(fps=60.0 rms=2.2ms heartbeat, confirmed via journalctl).
"""
import glob
import json
import os
import re
import subprocess
import time

WS_HOST, WS_PORT = "127.0.0.1", 4455
POLL_S = 5
BAD_FPS = 45.0
BAD_RMS = 25.0
CONSEC_TRIGGER = 4
DEAD_TRIGGER = 2
EVENTS = "/home/newlevel/imag-watchdog-events.log"
ALARMS = "/home/newlevel/imag-watchdog-alarms.jsonl"
ARCHIVE_DIR = "/home/newlevel/wedge-archive"
TEST_TRIGGER = "/home/newlevel/imag-watchdog-test-trigger"
SEED_SCRIPT = "/usr/local/bin/imag_scenes.py"
SENTINEL_GLOB = "/home/newlevel/.config/obs-studio/.sentinel/*"

from websocket import create_connection  # noqa: E402


def log(msg):
    line = f"{time.strftime('%Y-%m-%dT%H:%M:%S')} {msg}"
    print(line, flush=True)
    try:
        with open(EVENTS, "a") as f:
            f.write(line + "\n")
    except OSError as e:
        print(f"event-log write failed: {e}", flush=True)


def alarm(rec_type, detail):
    """Append one alarm record; the dev1 relay turns NEW ids into Discord pings."""
    rec = {"id": f"{time.strftime('%Y%m%dT%H%M%S')}-{rec_type}",
           "type": rec_type, "ts": time.time(),
           "iso": time.strftime("%Y-%m-%dT%H:%M:%S"), **detail}
    try:
        with open(ALARMS, "a") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        os.chmod(ALARMS, 0o644)
        log(f"ALARM RECORDED {rec['id']}")
    except OSError as e:
        log(f"ALARM WRITE FAILED ({rec_type}): {e}")


def obs_pid():
    try:
        out = subprocess.check_output(["pgrep", "-x", "obs"], text=True).split()
        return int(out[0]) if out else None
    except subprocess.CalledProcessError:
        return None


def ws_rpc(reqs, timeout=3):
    ws = create_connection(f"ws://{WS_HOST}:{WS_PORT}", timeout=timeout)
    try:
        json.loads(ws.recv())
        ws.send(json.dumps({"op": 1, "d": {"rpcVersion": 1, "eventSubscriptions": 0}}))
        json.loads(ws.recv())
        out = {}
        for t in reqs:
            ws.send(json.dumps({"op": 6, "d": {"requestType": t, "requestId": t, "requestData": {}}}))
            while True:
                m = json.loads(ws.recv())
                if m.get("op") == 7 and m["d"]["requestId"] == t:
                    out[t] = m["d"].get("responseData") or {}
                    break
        return out
    finally:
        try:
            ws.close()
        except Exception:
            pass  # socket already torn down


def sh(cmd, timeout=30):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return (p.stdout or "") + (("\n[STDERR]\n" + p.stderr) if p.stderr.strip() else "")
    except Exception as e:
        return f"[cmd failed: {e}]"


def graphics_tid(pid):
    """Find the 'libobs: graphic' render thread's tid (None if not found)."""
    try:
        for t in os.listdir(f"/proc/{pid}/task"):
            try:
                with open(f"/proc/{pid}/task/{t}/comm") as f:
                    if "graphic" in f.read():
                        return int(t)
            except OSError:
                continue  # thread exited mid-scan; benign race, keep scanning
    except OSError as e:
        log(f"graphics_tid scan failed for pid {pid}: {e}")
    return None


# ---------------------------------------------------------------------------------------------
# #849 -- hardware-aware wedge forensics. The old snapshot() unconditionally shelled nvidia-smi +
# hardcoded the incumbent GPU's PCI slot address, written for the incumbent box's discrete NVIDIA GPU
# (#756). The replacement notebook (#816) is Intel iGPU / i915 only, so those forensics produced
# useless noise and queried the WRONG PCI slot. These pure helpers select + build the box-
# appropriate command set; snapshot() executes them. All live-verified on the box (2026-08-14),
# never invented by analogy (the #841 rule).

# Mirrors scripts/setup-imag.sh's `imag_has_discrete_nvidia` bash function AND
# scripts/imag_scenes.py's Python copy EXACTLY (same regex, per-line, case-insensitive). That
# bash function cannot be imported here and imag_scenes.py deploys to a different dir not on this
# script's sys.path, so the regex is mirrored -- a parity test
# (tests/python/test_imag_obs_watchdog_snapshot_849.py) runs all copies against the SAME lspci
# fixtures so a future drift in any copy is caught (the #845/#847 established pattern).
_NVIDIA_DISCRETE_RE = re.compile(
    r"(vga compatible controller|3d controller|display controller).*nvidia", re.IGNORECASE
)
# A `lspci -nn` line begins with the device's PCI address (short form BB:DD.F, optional domain).
_PCI_ADDR_RE = re.compile(r"^((?:[0-9a-fA-F]{4}:)?[0-9a-fA-F]{2}:[0-9a-fA-F]{2}\.[0-9a-fA-F])\s")


def has_discrete_nvidia_from_lspci(lspci_output):
    """Pure -- True iff `lspci -nn` text names a discrete NVIDIA display-class device on any line."""
    return any(_NVIDIA_DISCRETE_RE.search(line) for line in lspci_output.splitlines())


def _display_pci_addr(lspci_output, vendor_re):
    for line in lspci_output.splitlines():
        if vendor_re.search(line):
            m = _PCI_ADDR_RE.match(line)
            if m:
                return m.group(1)
    return None


def nvidia_pci_addr_from_lspci(lspci_output):
    """Pure -- the DERIVED short PCI address of the discrete NVIDIA device, or None. #849: never
    the old hardcoded 01:00.0 (wrong slot on the replacement box)."""
    return _display_pci_addr(lspci_output, _NVIDIA_DISCRETE_RE)


_INTEL_DISPLAY_RE = re.compile(
    r"(vga compatible controller|3d controller|display controller).*intel", re.IGNORECASE
)


def intel_display_pci_addr_from_lspci(lspci_output):
    """Pure -- the DERIVED short PCI address of the Intel display controller, or None (used only
    for the no-dGPU i915 lspci LnkSta/AER probe)."""
    return _display_pci_addr(lspci_output, _INTEL_DISPLAY_RE)


def nvidia_forensic_cmds(pci_addr):
    """Pure -- (label, argv, timeout) forensic commands for a box WITH a discrete NVIDIA GPU. The
    runtime-PM / lspci probes use the lspci-DERIVED `pci_addr` (short BB:DD.F), never a hardcoded
    one (#849); the sysfs runtime_status path prepends the 0000: domain."""
    if pci_addr:
        sysfs = pci_addr if pci_addr.count(":") == 2 else "0000:" + pci_addr
        runtime_pm = (
            "cat /proc/driver/nvidia/gpus/*/power; "
            "echo runtime_status=$(cat /sys/bus/pci/devices/" + sysfs + "/power/runtime_status)"
        )
        lnk = (
            "lspci -vvv -s " + pci_addr
            + " 2>/dev/null | grep -EA2 'LnkCap|LnkSta|CESta|UESta|AERCap|Advanced Error'"
        )
    else:
        runtime_pm = "cat /proc/driver/nvidia/gpus/*/power; echo 'runtime_status: NVIDIA PCI address underivable from lspci'"
        lnk = "echo 'lspci LnkSta: NVIDIA PCI address underivable from lspci'"
    return [
        ("nvidia-smi -q -d PERFORMANCE (GSP-RPC discriminator; healthy ref ~0.02s)",
         ["nvidia-smi", "-q", "-d", "PERFORMANCE"], 20),
        ("nvidia-smi topline",
         ["nvidia-smi",
          "--query-gpu=pstate,utilization.gpu,clocks.gr,clocks.sm,power.draw,temperature.gpu,clocks_throttle_reasons.active",
          "--format=csv"], 15),
        ("nvidia-smi FB memory + clients",
         ["nvidia-smi", "-q", "-d", "MEMORY"], 15),
        ("fuser /dev/nvidia*",
         ["bash", "-c", "fuser -v /dev/nvidia* 2>&1 | head -20"], 10),
        ("runtime-PM state",
         ["bash", "-c", runtime_pm], 10),
        ("lspci LnkSta/LnkCap + AER",
         ["bash", "-c", lnk], 10),
    ]


def igpu_forensic_cmds(intel_pci_addr):
    """Pure -- (label, argv, timeout) forensic commands for a box with NO discrete GPU (Intel
    iGPU / i915). Live-verified surface (2026-08-14): globbed card*/gt/gt* rps_*_freq_mhz (act<<max
    => the #1040 power clamp), throttle_reason_* (1=active: the PL1/thermal/prochot discriminator),
    RAPL-mmio package-0 power envelope (MMIO wins over MSR), fuser /dev/dri (the render-node
    process holders), lspci LnkSta on the DERIVED Intel display address. DRM cards are GLOBBED (cardN
    renumbers). intel_gpu_top is deliberately EXCLUDED -- it core-dumps on this box's
    intel-gpu-tools 1.28 (get_num_gts assertion), so including it would be the #841
    invent-by-analogy trap; the render-thread kernel stacks + dmesg (both kept, generic below)
    carry the i915-hang signal instead."""
    if intel_pci_addr:
        lnk = (
            "lspci -vvv -s " + intel_pci_addr
            + " 2>/dev/null | grep -EA2 'LnkCap|LnkSta|CESta|UESta|AERCap|Advanced Error'"
        )
    else:
        lnk = "echo 'lspci LnkSta: Intel display PCI address underivable from lspci'"
    return [
        ("i915 GT frequency state (globbed card*/gt/gt* -- act<<max => clamp)",
         ["bash", "-c",
          "for f in /sys/class/drm/card*/gt/gt*/rps_act_freq_mhz "
          "/sys/class/drm/card*/gt/gt*/rps_cur_freq_mhz "
          "/sys/class/drm/card*/gt/gt*/rps_max_freq_mhz "
          "/sys/class/drm/card*/gt/gt*/rps_min_freq_mhz "
          "/sys/class/drm/card*/gt_act_freq_mhz /sys/class/drm/card*/gt_max_freq_mhz; do "
          "[ -r \"$f\" ] && echo \"$f = $(cat \"$f\")\"; done"], 10),
        ("i915 throttle reasons (1=active; #1040 PL1-clamp discriminator: pl1/thermal/prochot)",
         ["bash", "-c",
          "for f in /sys/class/drm/card*/gt/gt*/throttle_reason_*; do "
          "[ -r \"$f\" ] && echo \"$f = $(cat \"$f\")\"; done"], 10),
        ("RAPL package-0 power envelope (MMIO wins over MSR -- #1040)",
         ["bash", "-c",
          "for z in /sys/class/powercap/intel-rapl-mmio:*/; do "
          "n=$(cat \"$z/name\" 2>/dev/null); [ \"$n\" = package-0 ] || continue; "
          "echo \"$z name=$n\"; "
          "for c in \"$z\"constraint_*_name \"$z\"constraint_*_power_limit_uw \"$z\"energy_uj; do "
          "[ -r \"$c\" ] && echo \"  $c = $(cat \"$c\")\"; done; done"], 10),
        ("fuser /dev/dri render nodes (i915 render/GEM holders)",
         ["bash", "-c", "fuser -v /dev/dri/* 2>&1 | head -20"], 10),
        ("lspci LnkSta/LnkCap + AER (Intel display device)",
         ["bash", "-c", lnk], 10),
    ]


def select_forensic_cmds(lspci_output):
    """Pure branch -- (gpu_label, cmds) for the box `lspci_output` describes. Discrete NVIDIA ->
    the nvidia-smi forensics with a DERIVED address; otherwise the i915 surface (#849)."""
    if has_discrete_nvidia_from_lspci(lspci_output):
        return ("discrete NVIDIA", nvidia_forensic_cmds(nvidia_pci_addr_from_lspci(lspci_output)))
    return (
        "Intel iGPU / i915 (no discrete GPU)",
        igpu_forensic_cmds(intel_display_pci_addr_from_lspci(lspci_output)),
    )


def snapshot(reason):
    """Full passive autopsy capture (directive: the wedge is a live autopsy
    opportunity — capture EVERYTHING passively; in-vivo experiments are the
    HUMAN runbook's manual steps, never automated)."""
    ts = time.strftime("%Y%m%dT%H%M%S")
    d = os.path.join(ARCHIVE_DIR, ts)
    os.makedirs(d, exist_ok=True)
    pid = obs_pid()
    gtid = graphics_tid(pid) if pid else None
    t_deadline = time.time() + 90
    # #849: pick nvidia-smi vs i915 forensics from THIS box's actual hardware (lspci-derived),
    # never a hardcoded NVIDIA assumption. An unreadable lspci -> the i915 branch (this box's
    # known hardware) with the vendor labelled honestly.
    lspci_out = sh(["lspci", "-nn"], timeout=10)
    gpu_label, gpu_cmds = select_forensic_cmds(lspci_out)
    with open(os.path.join(d, "snapshot.txt"), "w") as f:
        f.write(f"### watchdog tier-b snapshot {ts} reason={reason} obs_pid={pid} graphics_tid={gtid} gpu={gpu_label}\n")
        # Every forensic command is timed -- the WALL time is the GSP-RPC/hang discriminator (a
        # healthy nvidia-smi -q -d PERFORMANCE returns ~0.02s; a wedged GPU or stuck sysfs read
        # blows up), and it applies uniformly to both the dGPU and i915 command sets.
        for label, argv, tmo in gpu_cmds:
            t0 = time.time()
            out = sh(argv, timeout=tmo)
            f.write(f"\n--- {label} (WALL {time.time()-t0:.2f}s) ---\n{out}\n")
        f.write("\n--- dmesg tail ---\n")
        f.write(sh(["bash", "-c", "dmesg -T 2>/dev/null | tail -30"], timeout=10))
        if gtid and time.time() < t_deadline:
            f.write(f"\n--- render-thread kernel stacks x20 (tid {gtid}, 100ms apart) ---\n")
            f.write(sh(["bash", "-c", f"for i in $(seq 1 20); do echo \"[$i] $(cat /proc/{pid}/task/{gtid}/stack 2>/dev/null | tr '\\n' '|')\"; sleep 0.1; done"], timeout=20))
            f.write(f"\n--- render-thread ioctl strace 3s (tid {gtid}, -c summary + -T sample) ---\n")
            f.write(sh(["timeout", "3", "strace", "-p", str(gtid), "-c"], timeout=10))
            f.write(sh(["bash", "-c", f"timeout 2 strace -p {gtid} -T -e trace=ioctl 2>&1 | tail -30"], timeout=8))
        if pid and time.time() < t_deadline:
            f.write(f"\n--- kernel stacks (all threads of {pid}) ---\n")
            f.write(sh(["bash", "-c", f"for t in /proc/{pid}/task/*/; do tid=$(basename $t); n=$(cat $t/comm 2>/dev/null); s=$(cat $t/stack 2>/dev/null | tr '\\n' '|'); echo \"$tid $n :: $s\"; done"], timeout=15))
        if pid and time.time() < t_deadline:
            f.write(f"\n--- gdb thread apply all bt ({pid}) ---\n")
            f.write(sh(["gdb", "-p", str(pid), "-batch", "-ex", "thread apply all bt"], timeout=max(5, int(t_deadline - time.time()))))
    return d


def clear_obs_sentinels():
    """#756 fix: clear OBS's own per-run .sentinel markers BEFORE relaunching.

    A tier-a relaunch follows a DEAD obs process -- by construction never a
    clean exit -- so the crashed run's sentinel file is always still present.
    OBS's own crash-recovery check then hangs the freshly-launched process at
    "Crash or unclean shutdown detected" (near-idle CPU, no further log
    progress) instead of actually starting, silently defeating the whole
    point of an auto-relaunching watchdog. Best-effort: a failure to remove a
    sentinel must never abort the relaunch attempt itself.
    """
    try:
        for f in glob.glob(SENTINEL_GLOB):
            try:
                os.remove(f)
            except OSError as e:
                log(f"sentinel clear: could not remove {f}: {e}")
    except OSError as e:
        log(f"sentinel clear: glob failed: {e}")


def relaunch_obs():
    log("tier-a: OBS dead -> relaunching as newlevel")
    clear_obs_sentinels()
    subprocess.run(["systemd-run", "--uid=newlevel", "--gid=newlevel",
                    "--setenv=DISPLAY=:0", "--setenv=XAUTHORITY=/home/newlevel/.Xauthority",
                    "--setenv=HOME=/home/newlevel",
                    "/usr/bin/taskset", "-c", "2-11", "/usr/bin/obs", "--disable-shutdown-check"])
    # wait for WS, then re-run the #522 self-heal (scenes + both projectors)
    deadline = time.time() + 90
    up = False
    while time.time() < deadline:
        try:
            ws_rpc(["GetVersion"])
            up = True
            break
        except Exception:
            time.sleep(3)
    if up:
        for args in ([SEED_SCRIPT, "--host", "127.0.0.1", "--bootstrap"],
                     [SEED_SCRIPT, "--host", "127.0.0.1", "--projector"]):
            subprocess.run(["systemd-run", "--uid=newlevel", "--gid=newlevel",
                            "--setenv=DISPLAY=:0", "--setenv=HOME=/home/newlevel",
                            "--wait", "/usr/bin/python3"] + args)
        log("tier-a: relaunch complete (WS up + scenes/projectors re-seeded)")
    else:
        log("tier-a: relaunch attempted but WS did not come up within 90s")
    return up


def boot_recovery_check():
    log("watchdog start (ALARM-ONLY mode — never reboots) — waiting for OBS+WS")
    deadline = time.time() + 240
    good = 0
    while time.time() < deadline and good < 3:
        try:
            st = ws_rpc(["GetStats"])["GetStats"]
            if float(st.get("activeFps", 0)) >= 58.0:
                good += 1
            else:
                good = 0
        except Exception:
            good = 0
        time.sleep(5)
    if good >= 3:
        log("STARTUP VERIFIED: fps>=58 x3")
    else:
        log("STARTUP NOT CONFIRMED within 240s — continuing to monitor")


def main():
    os.makedirs(ARCHIVE_DIR, exist_ok=True)
    boot_recovery_check()
    bad = 0
    dead = 0
    episode = False   # True after a tier-b alarm until recovery — one alarm per wedge
    good_streak = 0
    last_beat = 0.0
    while True:
        # test hook (delete BEFORE acting so a rehearsal cannot loop)
        if os.path.exists(TEST_TRIGGER):
            os.remove(TEST_TRIGGER)
            log("TEST TRIGGER -> tier-b ALARM-ONLY rehearsal (no reboot)")
            d = snapshot("test-trigger")
            alarm("tier-b", {"fps": -1, "rms": -1, "snapshot": d, "test": True})
            log(f"snapshot -> {d}; WAITING for human (no reboot, no output stops)")
            time.sleep(POLL_S)
            continue
        pid = obs_pid()
        if pid is None:
            dead += 1
            bad = 0
            if dead >= DEAD_TRIGGER:
                up = relaunch_obs()
                alarm("tier-a", {"relaunched": True, "ws_up": up})
                dead = 0
                time.sleep(20)
            else:
                time.sleep(POLL_S)
            continue
        dead = 0
        try:
            st = ws_rpc(["GetStats"])["GetStats"]
            fps = float(st.get("activeFps", 60.0))
            rms = float(st.get("averageFrameRenderTime", 0.0))
            rtot = float(st.get("renderTotalFrames", 0))
            is_bad = (fps < BAD_FPS or rms > BAD_RMS) and rtot > 3000
        except Exception:
            is_bad = True  # pid alive but WS dark = wedge-suspect
            fps = rms = -1.0
        bad = bad + 1 if is_bad else 0
        good_streak = good_streak + 1 if (not is_bad and fps >= 58.0) else 0
        if episode and good_streak >= 3:
            episode = False
            alarm("recovery", {"fps": fps, "rms": rms})
            log(f"EPISODE RECOVERED (fps={fps:.1f}) — re-armed")
        if time.time() - last_beat > 300:
            log(f"heartbeat fps={fps:.1f} rms={rms:.1f} bad={bad} episode={episode}")
            last_beat = time.time()
        if bad >= CONSEC_TRIGGER and not episode:
            log(f"TIER-B WEDGE: {CONSEC_TRIGGER} consecutive bad (fps={fps:.1f} rms={rms:.1f}) — snapshot + ALARM, then WAIT (no reboot)")
            d = snapshot(f"fps={fps:.1f},rms={rms:.1f}")
            alarm("tier-b", {"fps": fps, "rms": rms, "snapshot": d})
            episode = True
            log(f"snapshot -> {d}; WAITING for human decision")
        time.sleep(POLL_S)


if __name__ == "__main__":
    main()
