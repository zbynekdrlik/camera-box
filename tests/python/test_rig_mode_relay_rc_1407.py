"""issue 1407 follow-up (design addendum item 1, the issue-868 pattern): a failed bkshading relay step
must never stop a rig-mode switch half-way.

Since issue 1407 `bkshading_relay_mode_apply` returns non-zero when a relay box's root does not close
read-only again. rig-mode.sh called it bare under `set -euo pipefail`, so EVENT aborted BEFORE the
measurement burn went OFF (a burn stranded ON into a production, the issue-868 class), and TEST
aborted before the painter steps. Both modes now record the apply's rc, warn, run every remaining
step, and fold the rc into the exit status at the end, naming the relay box and its writers.

These tests RUN the real mode bodies: the flow section of scripts/rig-mode.sh (after its source
guard) is sourced next to the script, every function is stubbed to a step-logging no-op EXCEPT the
two mode bodies and the whole relay chain (the apply, its emitted remote text, the shared ro close
and the holder naming). The relay boxes are fake read-only-root boxes
(tests/python/ro_window_fakes_1407.py) behind a fake `sshpass` that routes `root@<ip>` to them, so
nothing leaves this host (TEST-NET-1 addresses, and an unknown address fails like a dead box).
Tier-0 (#557): no cargo, no rig, no network.
"""
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import ROOT, log, make_box, starts  # noqa: E402

RIG = ROOT / "scripts" / "rig-mode.sh"
SOURCE_IP = "192.0.2.61"   # the source camera (cam1) in these runs, TEST-NET-1
PAINTER_IP = "192.0.2.62"  # cam2
WRITER = "systemd-journal[76355]"  # the writer the fake box names when its ro close fails

# dev1-side fake `sshpass`: drops `-p <pass>`, finds `root@<ip>`, and RUNS the remote command on that
# ip's fake box (stub-only PATH). Per-box knobs come from <box>/fake.env (K=V lines).
_SSHPASS = r'''
import os, subprocess, sys
boxes = os.environ["FAKE_BOXES"]
ip = next((a.split("@", 1)[1] for a in sys.argv if a.startswith("root@")), "")
box = os.path.join(boxes, ip)
if not ip or not os.path.isdir(box):
    sys.stderr.write(f"ssh: connect to host {ip}: No route to host\n")
    sys.exit(255)
st = os.path.join(box, "state")
env = {"PATH": os.path.join(box, "bin"), "FAKE_STATE": st, "HOME": st}
knobs = os.path.join(box, "fake.env")
if os.path.exists(knobs):
    for line in open(knobs).read().splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            env[k] = v
sys.exit(subprocess.run(["/bin/bash", "-c", sys.argv[-1]], env=env).returncode)
'''

# The functions that stay REAL. Every other function the script defines becomes a step-logging stub.
# event_mode_discord_note_add is the one shared Discord-note writer the relay note calls.
_KEEP = "do_event|do_test|bkshading_relay_*|_bkshading_relay_*|ro_window_*|ro_root_*|event_mode_discord_note_add"


def _flow_text():
    s = RIG.read_text()
    return s[s.index("\nusage() {"):s.index('\nif [ "${BASH_SOURCE[0]}" = "${0}" ]; then')]


def _run(tmp_path, mode, failing=(), assert_pass=0, enabled=False, missing=(), extra=""):
    """Run the REAL do_<mode> body. FAILING = relay box ips whose ro close fails (a writer on /);
    MISSING = relay box ips with no box at all (the fake ssh answers like an unreachable host);
    EXTRA = bash run after the stubs (a test overrides one more step there)."""
    boxes = tmp_path / "boxes"
    boxes.mkdir()
    made = {}
    for ip in (SOURCE_IP, PAINTER_IP):
        if ip in missing:
            continue
        (boxes / ip).mkdir(parents=True)
        made[ip] = make_box(boxes / ip, root="ro")
        if enabled:
            (made[ip]["state"] / "enabled-bkshading-relay.service").write_text("enabled\n")
        if ip in failing:
            (boxes / ip / "fake.env").write_text("FAKE_RO_FAIL=1\n")
    dev1 = tmp_path / "dev1-bin"
    dev1.mkdir()
    (dev1 / "sshpass").write_text(f"#!{sys.executable}\n{_SSHPASS}")
    (dev1 / "sshpass").chmod(0o755)
    flow = tmp_path / "flow.sh"
    flow.write_text(_flow_text())
    steps, msg, sent = tmp_path / "steps", tmp_path / "discord-msg.txt", tmp_path / "discord-sent.txt"
    steps.write_text("")
    driver = tmp_path / "driver.sh"
    driver.write_text(f"""set -euo pipefail
STEPS="{steps}"
. "{RIG}"
. "{flow}"
RIG_SOURCE_BOX=cam1
RIG_SOURCE_IP={SOURCE_IP}
PAINTER_IP={PAINTER_IP}
for _f in $(declare -F | awk '{{print $3}}'); do
  case "$_f" in {_KEEP}) continue ;; esac
  eval "$_f() {{ _stub_step $_f \\"\\$@\\"; }}"
done
_stub_step() {{ printf 'STEP %s\\n' "$*" >>"$STEPS"; }}
event_mode_assert() {{
  _stub_step event_mode_assert
  EVENT_ASSERT_DISCORD_MSG_PATH="{msg}"
  EVENT_ASSERT_RESULT_JSON="{tmp_path / 'result.json'}"
  printf 'EVENT kontrakt potvrdeny\\n' >"$EVENT_ASSERT_DISCORD_MSG_PATH"
  EVENT_ASSERT_SUMMARY=summary
  EVENT_ASSERT_PASS={assert_pass}
}}
event_mode_discord_confirm_send() {{ _stub_step event_mode_discord_confirm_send; printf '%s' "$1" >"{sent}"; }}
{extra}
do_{mode}
""")
    env = {"PATH": f"{dev1}:/usr/bin:/bin", "FAKE_BOXES": str(boxes), "HOME": str(tmp_path)}
    proc = subprocess.run(["bash", str(driver)], env=env, capture_output=True, text=True, timeout=120)
    ran = [ln[len("STEP "):] for ln in steps.read_text().splitlines() if ln.startswith("STEP ")]
    return proc, ran, made, (sent.read_text() if sent.exists() else None)


def _at(ran, step):
    hits = [i for i, s in enumerate(ran) if s == step or s.startswith(step + " ")]
    assert hits, f"step {step!r} never ran; steps:\n" + "\n".join(ran)
    return hits[0]


def _relay_lines(text):
    return [ln for ln in text.splitlines() if "bkshading relay" in ln]


# ---- EVENT ---------------------------------------------------------------------------------------- #


def test_event_failed_relay_close_still_clears_the_burns_maps_and_runs_the_contract_then_fails(tmp_path):
    proc, ran, boxes, sent = _run(tmp_path, "event", failing=(SOURCE_IP,))
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    # every remaining EVENT step ran, in order, after the relay step
    order = [_at(ran, s) for s in ("camera_test_settings_restore", "event_artifact_purge_cmds", "toggle_burn event",
                                   "enforce_strih_ndi_mapping", "print_genlock_relaunch_note event",
                                   "event_mode_assert", "event_mode_discord_confirm_send")]
    assert order == sorted(order), "\n".join(ran)
    # the relay step itself ran on BOTH boxes: the healthy one started its relay on a read-only root,
    # the stuck one started nothing
    assert any(c.startswith("systemctl start bkshading-relay.service root=ro") for c in log(boxes[PAINTER_IP]))
    assert starts(log(boxes[SOURCE_IP])) == [], "\n".join(log(boxes[SOURCE_IP]))
    # loud while it continues, and the final verdict names the box and its writer
    warn = [ln for ln in proc.stderr.splitlines() if ln.startswith("WARNING") and "bkshading relay" in ln]
    assert warn, proc.stderr
    assert f"cam1 ({SOURCE_IP})" in warn[0] and WRITER in warn[0], warn[0]
    result = [ln for ln in proc.stderr.splitlines() if ln.startswith("RESULT:") and "bkshading relay" in ln]
    assert result, proc.stderr
    assert f"cam1 ({SOURCE_IP})" in result[0] and WRITER in result[0], result[0]
    assert f"cam2 ({PAINTER_IP})" not in result[0], result[0]
    # review round 1: never overstate -- a failed box can still run its relay, un-armed for reboot
    assert "is NOT running" not in result[0], result[0]
    assert "CONFIRMED clean for broadcast" not in out, out
    # the owner's EVENT Discord confirmation says it at the top, never a clean confirmation alone
    assert sent is not None, "the issue-724 confirmation must still be sent"
    first = sent.strip().splitlines()[0]
    assert first.startswith("⚠️") and f"cam1 ({SOURCE_IP})" in first, sent
    assert "burny vypnuté" not in first, "the note must not assert what the contract below decides:\n" + sent
    assert "EVENT kontrakt potvrdeny" in sent, sent


def test_event_with_every_relay_box_healthy_passes_clean(tmp_path):
    proc, ran, _boxes, sent = _run(tmp_path, "event")
    out = proc.stdout + proc.stderr
    assert proc.returncode == 0, out
    assert "CONFIRMED clean for broadcast" in proc.stdout, out
    assert _relay_lines(proc.stderr) == [], proc.stderr
    assert sent is not None and "⚠️" not in sent, sent


def test_event_relay_and_contract_both_failed_names_both(tmp_path):
    proc, _ran, _boxes, _sent = _run(tmp_path, "event", failing=(SOURCE_IP,), assert_pass=1)
    assert proc.returncode != 0
    assert [ln for ln in proc.stderr.splitlines() if ln.startswith("RESULT:") and "bkshading relay" in ln], proc.stderr
    assert "#722 CONTRACT FAILED" in proc.stderr, proc.stderr


def test_event_contract_failure_alone_still_reads_as_the_contract_failure(tmp_path):
    proc, _ran, _boxes, _sent = _run(tmp_path, "event", assert_pass=1)
    assert proc.returncode != 0
    assert "#722 CONTRACT FAILED" in proc.stderr, proc.stderr
    assert _relay_lines(proc.stderr) == [], proc.stderr


def test_event_an_unreachable_relay_box_is_the_same_record_and_fold(tmp_path):
    # not only a stuck root: a relay box that does not answer at all fails the step the same way
    proc, ran, _boxes, sent = _run(tmp_path, "event", missing=(SOURCE_IP,))
    assert proc.returncode != 0, proc.stdout + proc.stderr
    assert _at(ran, "toggle_burn event") < _at(ran, "event_mode_assert") < _at(ran, "event_mode_discord_confirm_send")
    result = [ln for ln in proc.stderr.splitlines() if ln.startswith("RESULT:") and "bkshading relay" in ln]
    assert result and f"cam1 ({SOURCE_IP})" in result[0], proc.stderr
    assert sent is not None and sent.startswith("⚠️"), sent


# ---- TEST ----------------------------------------------------------------------------------------- #


def test_test_failed_relay_close_still_runs_the_painter_steps_then_fails(tmp_path):
    proc, ran, boxes, _sent = _run(tmp_path, "test", failing=(SOURCE_IP,), enabled=True)
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    order = [_at(ran, s) for s in ("resolve_marker_device", "painter_launch_remote", "verify_marker_device_monitor",
                                   "toggle_burn test", "enforce_strih_ndi_mapping",
                                   "cam2_painter_steady_state_handoff_cmds", "cam2_painter_deadman_arm_cmds",
                                   "print_genlock_relaunch_note test")]
    assert order == sorted(order), "\n".join(ran)
    # the healthy box was disabled inside its window and closed read-only
    assert any(c.startswith("systemctl disable bkshading-relay.service root=rw") for c in log(boxes[PAINTER_IP]))
    result = [ln for ln in proc.stderr.splitlines() if ln.startswith("RESULT:") and "bkshading relay" in ln]
    assert result, proc.stderr
    assert f"cam1 ({SOURCE_IP})" in result[0] and WRITER in result[0], result[0]
    assert "WHOLE CHAIN verified" not in out, "a failed relay step must not end on the success verdict:\n" + out


def test_test_with_every_relay_box_healthy_passes(tmp_path):
    proc, _ran, _boxes, _sent = _run(tmp_path, "test", enabled=True)
    out = proc.stdout + proc.stderr
    assert proc.returncode == 0, out
    assert "WHOLE CHAIN verified" in proc.stdout, out
    assert _relay_lines(proc.stderr) == [], proc.stderr


def test_event_relay_failure_and_a_failed_cam_side_restore_name_both_and_still_clear_the_burns(tmp_path):
    # review round 1: the two recorded failures (the issue-868 cam-side restore + this relay step)
    # together -- the burn-OFF still runs, both RESULT lines are printed, the run exits non-zero.
    fail_first_cam_ssh = (
        'cam_ssh() { _stub_step cam_ssh; if [ ! -e "$STEPS.camfail" ]; then : >"$STEPS.camfail"; return 7; fi; }'
    )
    proc, ran, _boxes, _sent = _run(tmp_path, "event", failing=(SOURCE_IP,), extra=fail_first_cam_ssh)
    assert proc.returncode != 0, proc.stdout + proc.stderr
    assert _at(ran, "toggle_burn event") < _at(ran, "event_mode_assert"), "\n".join(ran)
    results = [ln for ln in proc.stderr.splitlines() if ln.startswith("RESULT:")]
    assert any("bkshading relay" in ln for ln in results), proc.stderr
    assert any("cam-side restore FAILED" in ln for ln in results), proc.stderr


def test_test_failed_relay_on_the_painter_box_with_a_read_write_root_stops_before_the_painter_launch(tmp_path):
    # ROZHODNUTÉ 5996845165 (Q1 = B): when the failed relay box IS the painter box (cam2) and its root
    # stayed read-WRITE, TEST stops BEFORE the painter launch. The painter handoff closes its own rw
    # window on that same root, so it can only fail; continuing would stop the running painter and
    # leave cam2 dark with no dead-man. TEST is development, so stopping strands nothing on air. The
    # run exits non-zero naming cam2 and its writers.
    proc, ran, _boxes, _sent = _run(tmp_path, "test", failing=(PAINTER_IP,), enabled=True)
    assert proc.returncode != 0, proc.stdout + proc.stderr
    for step in ("resolve_marker_device", "painter_launch_remote", "toggle_burn",
                 "cam2_painter_steady_state_handoff_cmds", "cam2_painter_deadman_arm_cmds"):
        assert not any(s == step or s.startswith(step + " ") for s in ran), f"{step} ran:\n" + "\n".join(ran)
    result = [ln for ln in proc.stderr.splitlines() if ln.startswith("RESULT:") and "bkshading relay" in ln]
    assert result and f"cam2 ({PAINTER_IP})" in result[0] and WRITER in result[0], proc.stderr
    assert "before the painter launch" in result[0], result[0]
    assert "continuing through" not in proc.stderr, "a stopped run must not announce that it continues:\n" + proc.stderr
    assert "WHOLE CHAIN verified" not in proc.stdout + proc.stderr


def test_test_unreachable_painter_box_still_runs_the_painter_steps(tmp_path):
    # Only a painter box whose root stayed read-WRITE stops TEST (Q1 = B); an unreachable painter box
    # (no verified close ran there) keeps the issue-868 continue-and-fold, like every other failure.
    proc, ran, _boxes, _sent = _run(tmp_path, "test", missing=(PAINTER_IP,), enabled=True)
    assert proc.returncode != 0, proc.stdout + proc.stderr
    assert _at(ran, "painter_launch_remote") < _at(ran, "cam2_painter_steady_state_handoff_cmds"), "\n".join(ran)
    result = [ln for ln in proc.stderr.splitlines() if ln.startswith("RESULT:") and "bkshading relay" in ln]
    assert result and f"cam2 ({PAINTER_IP})" in result[0], proc.stderr
    assert "before the painter launch" not in proc.stderr, proc.stderr


_DISCORD_LIB = ROOT / "scripts" / "lib" / "event-mode-discord-confirm.sh"


def _note_add(msg, line, where):
    script = f'. "{_DISCORD_LIB}"\nevent_mode_discord_note_add "$1" "$2" "$3"\necho RC=$?\n'
    return subprocess.run(["/bin/bash", "-c", "set -euo pipefail\n" + script, "h", str(msg), line, where],
                          capture_output=True, text=True, timeout=30)


def test_the_event_discord_note_is_one_shared_helper(tmp_path):
    # review round 1: the relay note copied the issue-1371 restore note's prepend-or-append code; both
    # now call the ONE helper in the confirmation lib.
    for rel in ("scripts/lib/camera-test-settings.sh", "scripts/lib/bkshading-relay-mode.sh"):
        text = (ROOT / rel).read_text()
        assert "event_mode_discord_note_add " in text, rel
        assert 'mv -f "$tmp" "$msg"' not in text, f"{rel} still carries its own prepend copy"
    msg = tmp_path / "msg.txt"
    msg.write_text("contract text\n")
    proc = _note_add(msg, "TOP LINE", "top")
    assert "RC=0" in proc.stdout, proc.stderr
    proc = _note_add(msg, "END LINE", "end")
    assert "RC=0" in proc.stdout, proc.stderr
    lines = [ln for ln in msg.read_text().splitlines() if ln.strip()]
    assert lines == ["TOP LINE", "contract text", "END LINE"], msg.read_text()
    assert [p.name for p in tmp_path.iterdir()] == ["msg.txt"], "no temp file is left behind"
    proc = _note_add(tmp_path / "absent" / "msg.txt", "X", "top")
    assert "RC=0" in proc.stdout and not (tmp_path / "absent").exists(), proc.stderr
