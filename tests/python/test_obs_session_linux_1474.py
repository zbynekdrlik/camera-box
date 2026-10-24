"""issue 1474 item 3 -- the obs-session watchdog watches the Linux OBS boxes between E2E runs.

Before: the obs-fleet `obs-session` facet was resolume-snv alone, and `obs_session_targets` skipped
every non-windows-genlock box, so nothing between two E2E runs checked that the Linux stream box
(stream-snv) -- or strih-snv -- still runs OBS for the operator. The `[0/8]` gate already has the
Linux check (`strih_linux_visibility_check`: `systemctl --user is-active strih-obs.service` over
plain ssh + a bounded obs-websocket round-trip). The watchdog now runs that same check on every
linux-genlock member, and the Windows obs64/AHK PowerShell probe stays for windows-genlock members.

Also pinned: the `win-baseline` facet (the Windows OBS-box baseline) takes only the windows-genlock
rows of its list, so the Linux stream-snv drops out of it (its rows read nothing), while a Windows
fleet override still carries both boxes.

Every test is offline: a fake `sshpass` (it plays both boxes by the remote text it is handed), a
fake `obs_phase2.py` dir (the OBS_SESSION_OBS_PHASE2_DIR seam) and a fake notify script.
"""
import os
import pathlib
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
WATCHDOG = SCRIPTS / "obs-session-watchdog.sh"
FLEET = SCRIPTS / "lib" / "obs-fleet.sh"

WIN_HEALTHY = ("ACTIVE_SESSION=1\nOWN_SESSION=0\nOBS_COUNT=1\nOBS_SESSION=1\nOBS_TITLE=\n"
               "AHK_COUNT=1\nAHK_SESSION=1\n")
WIN_STREAM_FLEET = ("strih-snv|10.77.9.202|linux-genlock|always\n"
                    "stream-snv|10.77.9.204|windows-genlock|always\n"
                    "resolume-snv|resolume.lan|windows-genlock|traveling")

FAKE_SSHPASS = r'''#!/usr/bin/env python3
"""Plays every box: logs the call, answers the Linux unit probe from FAKE_SVC (host=state ...),
the Windows PowerShell probe with a healthy session-1 reply. A host listed in FAKE_DOWN answers
nothing (rc 255), like a box ssh cannot reach."""
import os, sys
args = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("\x1f".join(args) + "\n")
host = next((a.split("@", 1)[1] for a in args[:-1] if "@" in a), "")
if host in os.environ.get("FAKE_DOWN", "").split():
    sys.exit(255)
remote = args[-1]
if "systemctl --user is-active strih-obs.service" in remote:
    states = dict(kv.split("=", 1) for kv in os.environ.get("FAKE_SVC", "").split())
    print("SVC_ACTIVE=" + states.get(host, "active"))
    sys.exit(0)
if "powershell" in " ".join(args).lower():
    sys.stdout.write(os.environ["FAKE_WIN_REPLY"])
    sys.exit(0)
sys.exit(97)
'''

FAKE_OBS_PHASE2 = r'''#!/usr/bin/env python3
"""The obs-websocket round-trip: `record --host H --action status`; rc 1 for a host in FAKE_WS_DOWN."""
import os, sys
host = sys.argv[sys.argv.index("--host") + 1]
with open(os.environ["FAKE_WS_LOG"], "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
sys.exit(1 if host in os.environ.get("FAKE_WS_DOWN", "").split() else 0)
'''

FAKE_NOTIFY = r'''import os, sys
with open(os.environ["FAKE_NOTIFY_LOG"], "a") as f:
    f.write("\x1f".join(sys.argv[1:]) + "\n")
'''


def _bash(script, env=None):
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True,
                          env={**os.environ, **(env or {})}, timeout=60)


OVERRIDES = ("OBS_FLEET", "OBS_SESSION_WATCHDOG_BOXES", "STRIH_SNV_HOST", "STREAM_SNV_HOST",
             "RESOLUME_SNV_HOST", "FAKE_SVC", "FAKE_DOWN", "FAKE_WS_DOWN")


def _run(tmp_path, args=(), **extra):
    """One watchdog pass over the REAL fleet (no OBS_FLEET), resolume forced home."""
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    sshpass = bindir / "sshpass"
    sshpass.write_text(FAKE_SSHPASS)
    sshpass.chmod(0o755)
    p2 = tmp_path / "p2"
    p2.mkdir(exist_ok=True)
    (p2 / "obs_phase2.py").write_text(FAKE_OBS_PHASE2)
    notify = tmp_path / "notify.py"
    notify.write_text(FAKE_NOTIFY)
    env = {k: v for k, v in os.environ.items() if k not in OVERRIDES}
    env.update({
        "PATH": f"{bindir}:{os.environ['PATH']}",
        "FAKE_LOG": str(tmp_path / "ssh.log"), "FAKE_WS_LOG": str(tmp_path / "ws.log"),
        "FAKE_NOTIFY_LOG": str(tmp_path / "notify.log"), "FAKE_WIN_REPLY": WIN_HEALTHY,
        "OBS_SESSION_OBS_PHASE2_DIR": str(p2), "AIRULESET_NOTIFY": str(notify),
        "OBS_SESSION_WATCHDOG_STATE_FILE": str(tmp_path / "state"),
        "OBS_FLEET_HOME": "resolume-snv",
    })
    env.update(extra)
    return subprocess.run(["bash", str(WATCHDOG), *args], capture_output=True, text=True,
                          env=env, timeout=120)


def _calls(tmp_path):
    p = tmp_path / "ssh.log"
    return [ln.split("\x1f") for ln in p.read_text().splitlines()] if p.exists() else []


def _notifies(tmp_path):
    p = tmp_path / "notify.log"
    return [ln.split("\x1f") for ln in p.read_text().splitlines()] if p.exists() else []


# --- the facets -----------------------------------------------------------------------------------

def test_obs_session_facet_carries_both_linux_snv_boxes_and_resolume():
    r = _bash(f'. "{FLEET}"; obs_fleet_facet_members obs-session; echo; obs_fleet_boxes obs-session',
              {"OBS_FLEET": ""})
    assert r.returncode == 0, r.stderr
    members, boxes = r.stdout.splitlines()
    assert members.split() == ["strih-snv", "stream-snv", "resolume-snv"]
    assert boxes == "strih-snv|10.77.9.202 stream-snv|10.77.9.204 resolume-snv|resolume.lan"


def test_win_baseline_facet_is_the_windows_rows_only():
    real = _bash(f'. "{FLEET}"; obs_fleet_facet_members win-baseline', {"OBS_FLEET": ""})
    assert real.returncode == 0, real.stderr
    assert real.stdout.split() == ["resolume-snv"], real.stdout
    win = _bash(f'. "{FLEET}"; obs_fleet_facet_members win-baseline', {"OBS_FLEET": WIN_STREAM_FLEET})
    assert win.returncode == 0, win.stderr
    assert win.stdout.split() == ["stream-snv", "resolume-snv"], win.stdout


# --- the targets ----------------------------------------------------------------------------------

def test_targets_on_the_real_fleet_send_the_linux_boxes_to_the_linux_probe():
    r = _bash(f'set --; . "{WATCHDOG}"; obs_session_targets',
              {"OBS_FLEET": "", "OBS_SESSION_WATCHDOG_BOXES": "", "OBS_FLEET_HOME": "resolume-snv",
               "STRIH_SNV_HOST": "", "STREAM_SNV_HOST": "", "RESOLUME_SNV_HOST": ""})
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == ["strih-snv 10.77.9.202 linux", "stream-snv 10.77.9.204 linux",
                                     "resolume-snv resolume.lan guard"]


def test_a_box_with_no_fleet_row_is_still_skipped():
    r = _bash(f'set --; . "{WATCHDOG}"; obs_session_targets',
              {"OBS_FLEET": "", "OBS_SESSION_WATCHDOG_BOXES": "mystery|192.0.2.5"})
    assert r.returncode == 0, r.stderr
    assert r.stdout == ""
    assert "mystery: skipped" in r.stderr


# --- a pass over the real fleet -------------------------------------------------------------------

def test_a_healthy_pass_probes_the_linux_boxes_over_plain_ssh_never_powershell(tmp_path):
    r = _run(tmp_path, ["--dry-run"])
    assert r.returncode == 0, r.stderr
    calls = _calls(tmp_path)
    for ip in ("10.77.9.202", "10.77.9.204"):
        mine = [c for c in calls if f"newlevel@{ip}" in c]
        assert len(mine) == 1, (ip, calls)
        assert "systemctl --user is-active strih-obs.service" in mine[0][-1], mine
        assert "UserKnownHostsFile=/dev/null" in mine[0], mine
        assert not any("powershell" in a.lower() for a in mine[0]), mine
    assert any("powershell" in " ".join(c).lower() for c in calls if "newlevel@resolume.lan" in c), calls
    ws = (tmp_path / "ws.log").read_text().splitlines()
    assert sorted(ws) == ["record --host 10.77.9.202 --action status",
                          "record --host 10.77.9.204 --action status"], ws
    for box in ("strih-snv", "stream-snv", "resolume-snv"):
        assert f"{box}: probe=" in r.stderr and f"{box}: confirm=0 -> 0" in r.stderr, r.stderr
    assert "wedged=1" not in r.stderr, r.stderr
    assert _notifies(tmp_path) == []


def test_obs_running_outside_its_unit_pages_once_confirmed_with_the_linux_recovery(tmp_path):
    # The one fault no other dev1 watchdog sees: obs-websocket answers (so obs-liveness reads the box
    # fine) while strih-obs.service is not active -- an OBS started outside its supervisor, not the
    # operator's kiosk instance (the Linux analogue of the Windows session-0 obs64).
    for _ in range(2):
        r = _run(tmp_path, FAKE_SVC="10.77.9.204=inactive")
        assert r.returncode == 0, r.stderr
    sent = _notifies(tmp_path)
    assert len(sent) == 1, sent
    args = sent[0]
    body = args[args.index("--body") + 1]
    assert args[args.index("--dedup-key") + 1] == "obs-session-stream-snv"
    assert "**stream-snv**" in body and "systemctl --user is-active -> inactive" in body, body
    assert "OBS answers on :4455 but is NOT its supervised strih-obs.service" in body, body
    assert "OBS is NOT running" not in body, body
    assert "ssh newlevel@10.77.9.204 'pgrep -a obs'" in body, body
    assert "ssh newlevel@10.77.9.204 'systemctl --user restart strih-obs.service'" in body, body
    assert "launch-obs-genlock" not in body and "win-*" not in body, body


def test_a_dead_websocket_is_left_to_obs_liveness_never_a_second_page(tmp_path):
    # obs-liveness (ENABLED on dev1, roster strih-snv stream-snv resolume-snv) already pages a dead
    # obs-websocket with the same unit-restart recovery. This watchdog decides nothing then: no
    # confirm, no page, whatever the unit reads -- one outage must reach the phone once.
    for svc in ("", "10.77.9.202=inactive"):
        state = tmp_path / f"state-{bool(svc)}"
        for _ in range(3):
            r = _run(tmp_path, FAKE_WS_DOWN="10.77.9.202", FAKE_SVC=svc,
                     OBS_SESSION_WATCHDOG_STATE_FILE=str(state))
            assert r.returncode == 0, r.stderr
        assert "strih-snv: obs-websocket on :4455 did not answer -- obs-liveness owns that page" in r.stderr, r.stderr
        assert "strih-snv: confirm=" not in r.stderr, r.stderr
        assert "stream-snv: confirm=0 -> 0" in r.stderr, r.stderr
    assert _notifies(tmp_path) == []


def test_a_dead_websocket_pass_breaks_the_consecutive_confirm(tmp_path):
    # "Confirmed over N consecutive passes": a dead-obs-websocket pass between two stray-OBS passes
    # is not one of them, so the confirm restarts and nothing pages on the second stray pass.
    for extra in ({"FAKE_SVC": "10.77.9.204=inactive"},
                  {"FAKE_SVC": "10.77.9.204=inactive", "FAKE_WS_DOWN": "10.77.9.204"},
                  {"FAKE_SVC": "10.77.9.204=inactive"}):
        r = _run(tmp_path, **extra)
        assert r.returncode == 0, r.stderr
    assert "stream-snv: confirm=0 -> 1" in r.stderr, r.stderr
    assert _notifies(tmp_path) == []


def test_an_unreachable_linux_box_is_connectivity_not_a_page(tmp_path):
    for _ in range(3):
        r = _run(tmp_path, FAKE_DOWN="10.77.9.204")
        assert r.returncode == 0, r.stderr
    assert "stream-snv: ERROR: no probe output" in r.stderr, r.stderr
    assert "stream-snv: confirm=" not in r.stderr, r.stderr
    assert _notifies(tmp_path) == []
    ws = (tmp_path / "ws.log").read_text().splitlines()
    assert not any("10.77.9.204" in w for w in ws), ws


def test_the_windows_box_keeps_its_byte_identical_alert(tmp_path):
    invisible = "ACTIVE_SESSION=1\nOWN_SESSION=0\nOBS_COUNT=1\nOBS_SESSION=0\nOBS_TITLE=OBS\n"
    for _ in range(2):
        r = _run(tmp_path, FAKE_WIN_REPLY=invisible, OBS_SESSION_WATCHDOG_BOXES="resolume-snv|resolume.lan")
        assert r.returncode == 0, r.stderr
    sent = _notifies(tmp_path)
    assert len(sent) == 1, sent
    body = sent[0][sent[0].index("--body") + 1]
    assert body.startswith("🚨 #979 obs-session-watchdog: **resolume-snv** obs64/AHK is INVISIBLE to the operator"), body
    assert body.endswith("Recovery (agent-driven, win-* MCP Shell): `bash scripts/launch-obs-genlock.sh --box resolume-snv --force`"), body


def test_the_unit_timeout_covers_a_pass_with_both_linux_boxes_hung():
    # Each Linux box costs up to the ssh bound plus the obs-websocket bound; a systemd kill mid-pass
    # would skip resolume and lose the later state writes. Sized from the script's own defaults, plus
    # 60 s for the Windows probe of resolume -- a SIZING, not a bound: win_ssh_run has only
    # ConnectTimeout=10 and no total limit (older than issue 1474).
    import re
    text = WATCHDOG.read_text()
    ssh = int(re.search(r'OBS_SESSION_LINUX_SSH_TIMEOUT:-(\d+)', text).group(1))
    ws = int(re.search(r'STRIH_LX_WS_TIMEOUT:-(\d+)', (SCRIPTS / "lib" / "strih-platform.sh").read_text()).group(1))
    unit = (REPO / "systemd" / "obs-session-watchdog.service").read_text()
    timeout = int(re.search(r'^TimeoutStartSec=(\d+)$', unit, re.M).group(1))
    assert timeout >= 2 * (ssh + ws) + 60, (timeout, ssh, ws)
