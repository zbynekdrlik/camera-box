"""#1302 slice 3 -- make the CG_CHAIN=1 run actually MEASURE (scripts/lib/cg-chain-e2e.sh).

Run 36622266165 (PR 1393) measured nothing: the SongPlayer burn ON never read back, so the cg OBS
hop burn correctly stayed OFF. Three defects, three fixes (design comment 5898583732, Approach 1):

  (a) the SongPlayer burn POST was discarded (`>/dev/null`), so 204 / 404 / 409 looked the same, and
      `/health` was read ONCE right after each POST although SongPlayer confirms "within 1 s". Now
      every POST logs its HTTP code + body, a 2xx is followed by a bounded POLL of `/health`, and a
      404 / 409 is a named failure that is never retried.
  (b) since SongPlayer plays its OWN program, a cg OBS cut over :4455 no longer starts anything in
      SongPlayer. Now SongPlayer's program is snapshotted and pressed through its obs-websocket
      facade (:4456), BOTH programs are read back before any burn goes on, and the snapshot is
      restored through the facade after the recording and in cleanup().
  (c) the cg OBS hop burn is saved in the cg OBS scene collection and no sweep covered that box.
      Now rig-mode EVENT and the E2E pre-run normalize sweep it while it is home (obs-fleet).

Everything is Tier-0: the lib is sourced under the caller's real `set -euo pipefail` and driven
against a FAKE `curl` (SongPlayer's burn / health / program API), FAKE `cg_chain_scene.py` /
`obs_phase2.py` / `obs_burn_filter.py` scripts run by the real python3, and a pass-through
`timeout`. rig-mode.sh is sourced with a fake `python3`. No SongPlayer, no OBS, no network.
"""
from __future__ import annotations

import json
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "cg-chain-e2e.sh"
_E2E = _ROOT / "scripts" / "recording-e2e.sh"
_RIG = _ROOT / "scripts" / "rig-mode.sh"

HOST = "10.77.9.201"
API = "http://resolume.lan:8920"

# Fake curl. Every call is logged to $CALL_LOG in order:
#   GET  .../api/v1/ndi/health  -> `curl GET health`; answers SP-fast `burn_on` from the comma list
#        $FAKE_HEALTH_SEQ, one value per call (the last one repeats);
#   GET  .../api/v1/program     -> `curl GET program`; answers the JSON in $FAKE_SP_PROGRAM_FILE (no
#        file = a refused connection, exit 7);
#   POST .../api/v1/ndi/burn    -> `curl POST <body>`; writes $FAKE_POST_BODY to the `-o` file and
#        prints $FAKE_POST_CODE (default 204) for `-w %{http_code}`; code 000 = no answer (curl's
#        error on stderr, exit 7).
_FAKE_CURL = r'''#!/usr/bin/env bash
url=""; body=""; ofile=""; prev=""
for a in "$@"; do
  case "$prev" in
    -d) body="$a" ;;
    -o) ofile="$a" ;;
  esac
  case "$a" in http*) url="$a" ;; esac
  prev="$a"
done
case "$url" in
  */api/v1/ndi/health)
    printf 'curl GET health\n' >> "$CALL_LOG"
    n=0; [ -f "$HEALTH_N" ] && n="$(cat "$HEALTH_N")"
    printf '%s' "$((n + 1))" > "$HEALTH_N"
    IFS=',' read -r -a seq <<< "${FAKE_HEALTH_SEQ:-false}"
    i="$n"; [ "$i" -lt "${#seq[@]}" ] || i=$(( ${#seq[@]} - 1 ))
    if [ "${seq[$i]}" = absent ]; then printf '[{"ndi_name":"SP-slow","burn_on":false}]'; else
      printf '[{"ndi_name":"SP-slow","burn_on":false},{"ndi_name":"SP-fast","burn_on":%s}]' "${seq[$i]}"; fi
    ;;
  */api/v1/program)
    printf 'curl GET program\n' >> "$CALL_LOG"
    if [ -f "${FAKE_SP_PROGRAM_FILE:-/nonexistent}" ]; then cat "$FAKE_SP_PROGRAM_FILE"; else
      echo "curl: (7) Failed to connect" >&2; exit 7; fi
    ;;
  */api/v1/program/cut)
    # SongPlayer's dashboard cut by source: the program moves to that source (no scene name).
    printf 'curl POST-CUT %s\n' "$body" >> "$CALL_LOG"
    if [ -z "${FAKE_CUT_NOOP:-}" ]; then
      python3 -c 'import json,sys; d=json.loads(sys.argv[1]); p=sys.argv[2]; json.dump({"source": d["source"], "remote": {"program_scene": None}}, open(p, "w"))' "$body" "$FAKE_SP_PROGRAM_FILE"
    fi
    if [ -n "$ofile" ]; then printf '%s' "${FAKE_CUT_BODY:-}" > "$ofile"; fi
    printf '%s' "${FAKE_CUT_CODE:-200}"
    ;;
  *)
    printf 'curl POST %s\n' "$body" >> "$CALL_LOG"
    code="${FAKE_POST_CODE:-204}"
    if [ "$code" = 000 ]; then echo "curl: (7) Failed to connect to resolume.lan port 8920" >&2; printf '000'; exit 7; fi
    if [ -n "$ofile" ]; then printf '%s' "${FAKE_POST_BODY:-}" > "$ofile"; fi
    printf '%s' "$code"
    ;;
esac
'''

# The scene helper: `program` (the :4455 cut, exit $FAKE_CUT_RC), `facade-program` (logs whether the
# SongPlayer snapshot already exists, then — unless $FAKE_FACADE_NOOP — moves SongPlayer's program
# to the pressed scene; exit $FAKE_FACADE_RC), `restore`.
_FAKE_SCENE = r'''
import json, os, sys
args = sys.argv[1:]
cmd = args[0]
def arg(name):
    return args[args.index(name) + 1]
snap = os.environ.get("SP_SNAPSHOT", "")
extra = ""
if cmd == "facade-program":
    extra = " snapshot=" + ("yes" if snap and os.path.exists(snap) else "no")
with open(os.environ["CALL_LOG"], "a") as f:
    f.write("cg_chain_scene.py " + " ".join(args) + extra + "\n")
if cmd == "program":
    sys.exit(int(os.environ.get("FAKE_CUT_RC", "0")))
if cmd == "facade-program":
    rc = int(os.environ.get("FAKE_FACADE_RC", "0"))
    if rc == 0 and not os.environ.get("FAKE_FACADE_NOOP"):
        # A press of a playlist scene: SP-program is cut to that playlist (its id) and published
        # with the scene name, like SongPlayer's switch_scene.
        path = os.environ["FAKE_SP_PROGRAM_FILE"]
        doc = json.load(open(path)) if os.path.exists(path) else {"source": None, "remote": {}}
        doc.setdefault("remote", {})["program_scene"] = arg("--scene")
        doc["source"] = json.loads(os.environ.get("FAKE_FACADE_SOURCE_MAP", "{}")).get(
            arg("--scene"), 7)
        json.dump(doc, open(path, "w"))
    sys.exit(rc)
'''

_FAKE_OBS_PHASE2 = r'''
import os, sys
with open(os.environ["CALL_LOG"], "a") as f:
    f.write("obs_phase2.py " + " ".join(sys.argv[1:]) + "\n")
if sys.argv[1] == "program-scene":
    lag = os.environ.get("FAKE_CG_LAG_FILE")
    if lag and not os.path.exists(lag):
        open(lag, "w").close()
        print("sp-slow")  # the facade's mirror has not landed on cg OBS yet
    else:
        print(os.environ.get("FAKE_CG_PROGRAM", "sp-fast"))
if "start" in sys.argv:
    sys.exit(int(os.environ.get("FAKE_START_RC", "0")))
'''

_FAKE_BURN_FILTER = r'''
import os, sys
with open(os.environ["CALL_LOG"], "a") as f:
    f.write("obs_burn_filter.py " + " ".join(sys.argv[1:]) + "\n")
action = sys.argv[1]
if action == "check":
    state = open(os.environ["BURN_STATE_FILE"]).read() if os.path.exists(os.environ["BURN_STATE_FILE"]) else "off"
    on = state == "on"
    print(f"[burn] burn_on={on} genlock_burn={on} filter_on_input=True filter_enabled=True "
          f"kind_registered=True input='x'")
elif action == "add":
    open(os.environ["BURN_STATE_FILE"], "w").write("on")
elif action == "remove":
    open(os.environ["BURN_STATE_FILE"], "w").write("off")
elif action == "sweep-off":
    print(f"[sweep] host={sys.argv[3]} no ndi input had genlock_burn ON (4 scanned)")
    sys.exit(int(os.environ.get("FAKE_SWEEP_RC", "0")))
'''

_FAKE_TIMEOUT = r'''#!/usr/bin/env bash
printf '%s\n' "$1" >> "$TIMEOUT_LOG"
shift
exec "$@"
'''


def _program_doc(scene, source):
    return json.dumps({"ndi_name": "SP-program", "source": source,
                       "remote": {"program_scene": scene}})


def _run(tmp_path, snippet, env=None, sp_program=("sp-fast", 7)):
    """Source the lib under `set -euo pipefail` with every fake installed and run `snippet`.

    `sp_program` = SongPlayer's program before the run as (scene, source), or None for an
    unreachable program API. Returns (rc, stdout, stderr, calls)."""
    scripts = tmp_path / "scripts"
    scripts.mkdir(exist_ok=True)
    (scripts / "cg_chain_scene.py").write_text(_FAKE_SCENE)
    (scripts / "obs_phase2.py").write_text(_FAKE_OBS_PHASE2)
    (scripts / "obs_burn_filter.py").write_text(_FAKE_BURN_FILTER)
    fbin = tmp_path / "bin"
    fbin.mkdir(exist_ok=True)
    for name, body in (("curl", _FAKE_CURL), ("timeout", _FAKE_TIMEOUT)):
        p = fbin / name
        p.write_text(body)
        p.chmod(0o755)
    call_log = tmp_path / "calls.log"
    call_log.touch()
    program_file = tmp_path / "sp-program.json"
    if sp_program is not None:
        program_file.write_text(_program_doc(*sp_program))
    full_env = {
        "PATH": f"{fbin}:/usr/local/bin:/usr/bin:/bin",
        "HOME": str(tmp_path),
        "TMPDIR": str(tmp_path),
        "CALL_LOG": str(call_log),
        "TIMEOUT_LOG": str(tmp_path / "timeout.log"),
        "HEALTH_N": str(tmp_path / "health.n"),
        "BURN_STATE_FILE": str(tmp_path / "burn.state"),
        "FAKE_SP_PROGRAM_FILE": str(program_file),
        "SP_SNAPSHOT": str(tmp_path / "cg-chain-sp-program-state.json"),
        "CG_CHAIN_STATE_DIR": str(tmp_path),
        "CG_CHAIN_BURN_RETRY_SLEEP": "0",
        "CG_CHAIN_READBACK_POLL_MS": "20",
        "CG_CHAIN_BURN_READBACK_S": "1",
        "CG_CHAIN_PROGRAM_READBACK_S": "0",
        "PY": str(scripts / "obs_phase2.py"),
        "BF": str(scripts / "obs_burn_filter.py"),
        "HOST": HOST,
        "OBS_FLEET_HOME": "none",
        "FAKE_FACADE_SOURCE_MAP": json.dumps({"sp-fast": 7, "sp-slow": 3}),
    }
    if env:
        full_env.update(env)
    script = f'set -euo pipefail\n. "{_LIB}"\n{snippet}\n'
    out = subprocess.run(["/bin/bash", "-c", script], env=full_env, capture_output=True,
                         text=True, check=False)
    calls = [ln for ln in call_log.read_text().splitlines() if ln]
    return out.returncode, out.stdout, out.stderr, calls


def _posts(calls):
    """The burn toggle POSTs (never the dashboard program cut, logged as `curl POST-CUT`)."""
    return [c for c in calls if c.startswith("curl POST ")]


def _health_reads(calls):
    return [c for c in calls if c == "curl GET health"]


# ---- (a) the burn toggle: HTTP diagnostics + a bounded read-back poll --------------------------


def test_a_204_is_polled_until_burn_on_reads_true_on_the_second_read(tmp_path):
    rc, out, err, calls = _run(
        tmp_path, 'cg_chain_songplayer_burn on; printf "SP=%s OWED=%s\\n" "$CG_SP_BURN_ON" "$CG_SP_BURN_OWED"',
        env={"FAKE_HEALTH_SEQ": "false,true"})
    assert rc == 0, err
    assert len(_posts(calls)) == 1, f"a late read-back is polled, never re-POSTed: {calls}"
    assert len(_health_reads(calls)) == 2, calls
    assert "SongPlayer burn on attempt 1: HTTP 204" in out, out
    assert "SongPlayer burn on VERIFIED" in out and "SP=1 OWED=1" in out, out


def test_every_post_logs_its_http_code_and_the_body_cut_to_200_chars(tmp_path):
    long_body = "x" * 150 + "\n" + "y" * 150
    rc, out, err, _ = _run(
        tmp_path, "CG_CHAIN_BURN_ATTEMPTS=1 cg_chain_songplayer_burn on",
        env={"FAKE_POST_CODE": "500", "FAKE_POST_BODY": long_body, "FAKE_HEALTH_SEQ": "false"})
    assert rc == 0, err
    line = next(ln for ln in out.splitlines() if "attempt 1: HTTP 500" in ln)
    shown = line.split("HTTP 500 ", 1)[1]
    assert len(shown) == 200 and "\n" not in shown and shown.startswith("x" * 150 + " y"), line


def test_a_404_is_named_and_never_retried(tmp_path):
    rc, out, err, calls = _run(
        tmp_path, 'cg_chain_songplayer_burn on; printf "SP=%s\\n" "$CG_SP_BURN_ON"',
        env={"FAKE_POST_CODE": "404", "FAKE_POST_BODY": "unknown output"})
    assert rc == 0, err
    assert len(_posts(calls)) == 1, f"a 404 is a final answer: {calls}"
    assert "attempt 1: HTTP 404 unknown output" in out, out
    assert "HTTP 404: SongPlayer has no output 'SP-fast'" in err and "not retried" in err, err
    assert "SP=0" in out


def test_a_409_is_named_and_never_retried(tmp_path):
    rc, out, err, calls = _run(
        tmp_path, "cg_chain_songplayer_burn on",
        env={"FAKE_POST_CODE": "409", "FAKE_POST_BODY": "pacing disabled"})
    assert rc == 0, err
    assert len(_posts(calls)) == 1, calls
    assert "HTTP 409: pacing is disabled on 'SP-fast'" in err and "not retried" in err, err


def test_an_off_answered_404_owes_nothing(tmp_path):
    # No pipeline has the output, so /health has no row for it either (`unknown`, never `false`).
    rc, out, err, calls = _run(
        tmp_path, 'CG_SP_BURN_OWED=1; cg_chain_songplayer_burn off; printf "OWED=%s\\n" "$CG_SP_BURN_OWED"',
        env={"FAKE_POST_CODE": "404", "FAKE_HEALTH_SEQ": "absent"})
    assert rc == 0, err
    assert len(_posts(calls)) == 1
    assert "nothing to turn off" in out and "OWED=0" in out, out
    assert "LEAK" not in err


def test_an_off_answered_409_is_confirmed_by_one_read(tmp_path):
    rc, out, err, calls = _run(
        tmp_path, 'cg_chain_songplayer_burn off; printf "OWED=%s\\n" "$CG_SP_BURN_OWED"',
        env={"FAKE_POST_CODE": "409", "FAKE_HEALTH_SEQ": "false"})
    assert rc == 0, err
    assert len(_posts(calls)) == 1 and len(_health_reads(calls)) == 1, calls
    assert "SongPlayer burn off VERIFIED" in out and "OWED=0" in out, out


def test_an_off_that_never_reads_false_is_a_leak_line_and_keeps_the_owed_flag(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN_BURN_ATTEMPTS=2 cg_chain_songplayer_burn off; echo REACHED; printf "OWED=%s\\n" "$CG_SP_BURN_OWED"',
        env={"FAKE_HEALTH_SEQ": "true", "CG_CHAIN_BURN_READBACK_S": "0"})
    assert rc == 0 and "REACHED" in out
    assert len(_posts(calls)) == 2, calls
    assert "LEAK: SongPlayer burn still not OFF after 2 attempt(s)" in err, err
    assert "OWED=1" in out


def test_no_http_answer_is_logged_with_curls_error_and_never_read_back(tmp_path):
    rc, out, err, calls = _run(
        tmp_path, "CG_CHAIN_BURN_ATTEMPTS=2 cg_chain_songplayer_burn on",
        env={"FAKE_POST_CODE": "000"})
    assert rc == 0, err
    assert "attempt 1: HTTP 000 curl: (7) Failed to connect" in out, out
    assert _health_reads(calls) == [], "no HTTP answer = nothing to read back"
    assert len(_posts(calls)) == 2, "a transport failure is retried"


def test_the_read_back_poll_is_bounded_by_wall_time(tmp_path):
    # 1 s budget at 200 ms: at most 1 + 5 reads per accepted POST, never an unbounded loop.
    rc, _, err, calls = _run(
        tmp_path, "CG_CHAIN_BURN_ATTEMPTS=1 cg_chain_songplayer_burn on",
        env={"FAKE_HEALTH_SEQ": "false", "CG_CHAIN_READBACK_POLL_MS": "200",
             "CG_CHAIN_BURN_READBACK_S": "1"})
    assert rc == 0, err
    assert 2 <= len(_health_reads(calls)) <= 6, calls
    assert "not confirmed after 1 attempt(s)" in err


def test_poll_knobs_default_and_reject_garbage(tmp_path):
    snippet = r'''
unset CG_CHAIN_BURN_READBACK_S CG_CHAIN_PROGRAM_READBACK_S CG_CHAIN_READBACK_POLL_MS CG_CHAIN_SP_FACADE_PORT
printf '%s %s %s %s\n' "$(cg_chain_burn_readback_secs)" "$(cg_chain_program_readback_secs)" "$(cg_chain_readback_poll_ms)" "$(cg_chain_sp_facade_port)"
CG_CHAIN_BURN_READBACK_S=x CG_CHAIN_PROGRAM_READBACK_S=-1 CG_CHAIN_READBACK_POLL_MS=0 CG_CHAIN_SP_FACADE_PORT=abc
printf '%s %s %s %s\n' "$(cg_chain_burn_readback_secs)" "$(cg_chain_program_readback_secs)" "$(cg_chain_readback_poll_ms)" "$(cg_chain_sp_facade_port)"
CG_CHAIN_BURN_READBACK_S=08 CG_CHAIN_READBACK_POLL_MS=250 CG_CHAIN_SP_FACADE_PORT=4457
printf '%s %s %s\n' "$(cg_chain_burn_readback_secs)" "$(cg_chain_readback_poll_ms)" "$(cg_chain_sp_facade_port)"
'''
    rc, out, err, _ = _run(tmp_path, snippet)
    assert rc == 0, err
    assert out.splitlines() == ["3 5 500 4456", "3 5 500 4456", "8 250 4457"]


# ---- (b) SongPlayer's own program: snapshot, facade cut, both programs read back -----------------


def test_program_fields_reads_scene_and_source(tmp_path):
    snippet = r'''
printf '%s' '{"source":7,"remote":{"program_scene":"sp-fast"}}' | cg_chain_program_fields; echo "|"
printf '%s' '{"source":null,"remote":{"program_scene":null}}' | cg_chain_program_fields; echo "|"
printf '%s' '{"source":-1}' | cg_chain_program_fields; echo "|"
if printf 'not json' | cg_chain_program_fields; then echo PARSED; else echo REJECTED; fi
if printf '[1]' | cg_chain_program_fields; then echo PARSED; else echo REJECTED; fi
'''
    rc, out, err, _ = _run(tmp_path, snippet)
    assert rc == 0, err
    assert out.splitlines() == ["sp-fast\t7|", "\t|", "\t-1|", "REJECTED", "REJECTED"]


def test_record_start_cuts_both_programs_reads_both_back_then_burns(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1\nif cg_chain_record_start "$HOST" "$PY" 30; then echo STARTED; fi\n'
        'printf "SP=%s CG=%s\\n" "$CG_SP_BURN_ON" "${CG_BURN_ON:-unset}"',
        env={"FAKE_HEALTH_SEQ": "true"}, sp_program=("sp-slow", 3))
    assert rc == 0, err
    assert "STARTED" in out and "SP=1 CG=1" in out, out + err
    idx = {name: next(i for i, c in enumerate(calls) if c.startswith(prefix)) for name, prefix in (
        ("cg_cut", "cg_chain_scene.py program --host"),
        ("facade", "cg_chain_scene.py facade-program"),
        ("cg_read", "obs_phase2.py program-scene"),
        ("sp_post", "curl POST"),
        ("cg_add", "obs_burn_filter.py add"),
        ("start", "obs_phase2.py record"),
    )}
    assert idx["cg_cut"] < idx["facade"] < idx["cg_read"] < idx["sp_post"] < idx["cg_add"] < idx["start"], calls
    assert f"cg_chain_scene.py facade-program --host {HOST} --port 4456 --scene sp-fast snapshot=yes" in calls, (
        "SongPlayer's program is snapshotted BEFORE the facade press")
    snap = json.loads((tmp_path / "cg-chain-sp-program-state.json").read_text())
    assert snap == {"host": HOST, "port": 4456, "scene": "sp-slow", "source": 3}


def test_record_start_re_kicks_a_playlist_already_on_air_without_a_snapshot(tmp_path):
    # review round 1: SongPlayer's own design -- a press of the scene already on air re-kicks a
    # playlist paused out of band (program_on_air.rs on_air_changes; ProgramCore::cut is a no-op on
    # the same source). The program does not change, so there is nothing to snapshot or restore.
    rc, out, err, calls = _run(
        tmp_path, 'CG_CHAIN=1\nif cg_chain_record_start "$HOST" "$PY" 30; then echo STARTED; fi',
        env={"FAKE_HEALTH_SEQ": "true"}, sp_program=("sp-fast", 7))
    assert rc == 0, err
    assert f"cg_chain_scene.py facade-program --host {HOST} --port 4456 --scene sp-fast snapshot=no" in calls
    assert not (tmp_path / "cg-chain-sp-program-state.json").exists(), "nothing to undo = no snapshot"
    assert "already on air" in out and "re-kick" in out and _posts(calls), out


def test_a_songplayer_program_mismatch_keeps_both_burns_off(tmp_path):
    # SongPlayer answers the press but its program never moves (e.g. the scene is not a playlist).
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1\nif cg_chain_record_start "$HOST" "$PY" 30; then echo STARTED; fi\n'
        'printf "SP=%s CG=%s\\n" "${CG_SP_BURN_ON:-unset}" "${CG_BURN_ON:-unset}"',
        env={"FAKE_FACADE_NOOP": "1", "FAKE_HEALTH_SEQ": "true"}, sp_program=("sp-slow", 3))
    assert rc == 0, err
    assert "program mismatch after the cut" in err and "SongPlayer program 'sp-slow'" in err, err
    assert _posts(calls) == [], "no program read back = the SongPlayer burn is never turned on"
    assert not any(c.startswith("obs_burn_filter.py add") for c in calls)
    assert "STARTED" in out and "SP=unset CG=unset" in out, out


def test_a_cg_obs_program_mismatch_keeps_both_burns_off(tmp_path):
    rc, out, err, calls = _run(
        tmp_path, 'CG_CHAIN=1\nif cg_chain_record_start "$HOST" "$PY" 30; then echo STARTED; fi',
        env={"FAKE_CG_PROGRAM": "CG bridge", "FAKE_HEALTH_SEQ": "true"})
    assert rc == 0, err
    assert "cg OBS program 'CG bridge', want 'sp-fast'" in err, err
    assert _posts(calls) == [] and "STARTED" in out


def test_an_unreadable_songplayer_program_is_never_pressed(tmp_path):
    rc, out, err, calls = _run(
        tmp_path, 'CG_CHAIN=1\nif cg_chain_record_start "$HOST" "$PY" 30; then echo STARTED; fi',
        env={"FAKE_HEALTH_SEQ": "true"}, sp_program=None)
    assert rc == 0, err
    assert "could not read SongPlayer's program" in err, err
    assert not any(c.startswith("cg_chain_scene.py facade-program") for c in calls)
    assert not (tmp_path / "cg-chain-sp-program-state.json").exists()
    assert _posts(calls) == [] and "STARTED" in out


def test_the_program_read_back_polls_until_the_mirror_lands(tmp_path):
    # The cg OBS read lags one poll behind (the facade's mirror is not awaited by SongPlayer).
    rc, out, err, calls = _run(
        tmp_path, 'if cg_chain_program_readback "$HOST" "$PY"; then echo MATCH; fi',
        env={"CG_CHAIN_PROGRAM_READBACK_S": "2", "FAKE_CG_LAG_FILE": str(tmp_path / "lag")})
    assert rc == 0, err
    assert "MATCH" in out and "programs read back: SongPlayer 'sp-fast' (source 7) + cg OBS" in out, out + err
    assert sum(c.startswith("obs_phase2.py program-scene") for c in calls) == 2, calls


def test_after_stoprecord_restores_songplayer_through_the_facade_before_the_cg_restore(tmp_path):
    (tmp_path / "cg-chain-cg-program-state.json").write_text("{}")
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": "sp-slow", "source": 3}))
    rc, out, err, calls = _run(
        tmp_path, 'CG_CHAIN=1 CG_RECORDING_STARTED=0\ncg_chain_after_stoprecord "$HOST" "$PY" 30\n'
                  'cg_chain_cleanup "$HOST" "$PY" 30',
        env={"FAKE_HEALTH_SEQ": "false"}, sp_program=("sp-fast", 7))
    assert rc == 0, err
    press = calls.index(f"cg_chain_scene.py facade-program --host {HOST} --port 4456 --scene sp-slow snapshot=yes")
    cg_restore = next(i for i, c in enumerate(calls) if c.startswith("cg_chain_scene.py restore"))
    assert press < cg_restore, f"cg OBS is restored LAST, over :4455: {calls}"
    assert "SongPlayer program restored -> 'sp-slow'" in out, out
    assert (tmp_path / "cg-chain-sp-program-state.json.restored").exists()
    assert sum(c.startswith("cg_chain_scene.py facade-program") for c in calls) == 1, \
        "the cleanup() pass after a restore is a no-op"


def test_cleanup_restores_songplayer_after_an_early_abort(tmp_path):
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": "sp-slow", "source": 3}))
    rc, out, err, calls = _run(tmp_path, 'CG_CHAIN=1\ncg_chain_cleanup "" "$PY" 30',
                               env={"FAKE_HEALTH_SEQ": "false"}, sp_program=("sp-fast", 7))
    assert rc == 0, err
    assert f"cg_chain_scene.py facade-program --host {HOST} --port 4456 --scene sp-slow snapshot=yes" in calls


def test_restore_does_not_press_a_program_that_is_already_back(tmp_path):
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": "sp-slow", "source": 3}))
    rc, out, err, calls = _run(tmp_path, 'cg_chain_sp_program_restore "$PY" 30',
                               sp_program=("sp-slow", 3))
    assert rc == 0, err
    assert not any(c.startswith("cg_chain_scene.py facade-program") for c in calls)
    assert "already back on 'sp-slow'" in out
    assert (tmp_path / "cg-chain-sp-program-state.json.restored").exists()


def test_restore_without_a_scene_cuts_by_source_and_reads_it_back(tmp_path):
    # review round 1: a snapshot with no scene name (a scene-less playlist, or the NDI input -1)
    # cannot be pressed through the facade; SongPlayer's dashboard cut by SOURCE (the same
    # switch_source path) restores it, and the source is read back before the snapshot retires.
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": None, "source": 5}))
    rc, out, err, calls = _run(tmp_path, 'cg_chain_sp_program_restore "$PY" 30')
    assert rc == 0, err
    assert not any(c.startswith("cg_chain_scene.py facade-program") for c in calls)
    assert 'curl POST-CUT {"source":5}' in calls, calls
    assert "SongPlayer program restored -> source 5" in out and "HTTP 200" in out, out
    assert (tmp_path / "cg-chain-sp-program-state.json.restored").exists()


def test_restore_of_a_scene_less_cut_that_never_reads_back_keeps_the_snapshot(tmp_path):
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": None, "source": 5}))
    rc, out, err, calls = _run(tmp_path, 'cg_chain_sp_program_restore "$PY" 30; echo REACHED',
                               env={"FAKE_CUT_NOOP": "1"})
    assert rc == 0 and "REACHED" in out
    assert "did not read back" in err and f"{API}/api/v1/program/cut" in err, err
    assert (tmp_path / "cg-chain-sp-program-state.json").exists()


def test_restore_with_nothing_on_program_before_the_run_names_it_and_keeps_the_snapshot(tmp_path):
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": None, "source": None}))
    rc, out, err, calls = _run(tmp_path, 'cg_chain_sp_program_restore "$PY" 30')
    assert rc == 0, err
    assert not any("POST-CUT" in c or "facade-program" in c for c in calls), calls
    assert "nothing was on SongPlayer's program before this run" in err, err
    assert (tmp_path / "cg-chain-sp-program-state.json").exists()


def test_a_failed_restore_press_keeps_the_snapshot_and_names_the_command(tmp_path):
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": "sp-slow", "source": 3}))
    rc, out, err, _ = _run(tmp_path, 'cg_chain_sp_program_restore "$PY" 30; echo REACHED',
                           env={"FAKE_FACADE_RC": "1"})
    assert rc == 0 and "REACHED" in out
    assert "restore through the facade failed" in err and "--port 4456 --scene 'sp-slow'" in err, err
    assert (tmp_path / "cg-chain-sp-program-state.json").exists()


def test_disabled_profile_never_restores_a_songplayer_snapshot(tmp_path):
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": "sp-slow", "source": 3}))
    rc, out, err, calls = _run(
        tmp_path, 'unset CG_CHAIN\ncg_chain_after_stoprecord "$HOST" "$PY" 5\n'
                  'cg_chain_cleanup "$HOST" "$PY" 5\necho DONE')
    assert rc == 0, err
    assert calls == [] and out.strip() == "DONE" and err == ""


# ---- review round 1 (the fresh-context review of this slice) ----------------------------------


def test_an_off_answered_404_while_health_reads_true_is_a_leak(tmp_path):
    # SongPlayer's registry also answers NotFound on a poisoned lock; a `true` read is the truth.
    rc, out, err, calls = _run(
        tmp_path, 'CG_SP_BURN_OWED=0; cg_chain_songplayer_burn off; printf "OWED=%s\\n" "$CG_SP_BURN_OWED"',
        env={"FAKE_POST_CODE": "404", "FAKE_HEALTH_SEQ": "true"})
    assert rc == 0, err
    assert "LEAK" in err and "HTTP 404" in err, err
    assert "nothing to turn off" not in out
    assert "OWED=1" in out


def test_a_manual_program_carrying_the_scene_name_is_not_the_playlist_on_air(tmp_path):
    # SongPlayer's switch_manual cuts SP-program to the NDI input (source -1) and still publishes
    # the scene name, so `program_scene` alone reads sp-fast while no playlist plays.
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1\nif cg_chain_record_start "$HOST" "$PY" 30; then echo STARTED; fi\n'
        'printf "SP=%s\\n" "${CG_SP_BURN_ON:-unset}"',
        env={"FAKE_FACADE_NOOP": "1", "FAKE_HEALTH_SEQ": "true"}, sp_program=("sp-fast", -1))
    assert rc == 0, err
    snap = json.loads((tmp_path / "cg-chain-sp-program-state.json").read_text())
    assert snap == {"host": HOST, "port": 4456, "scene": "sp-fast", "source": -1}, (
        "not on air: the program is snapshotted and pressed")
    assert "program mismatch after the cut" in err and "source -1" in err, err
    assert _posts(calls) == [] and "SP=unset" in out


def test_the_program_read_back_needs_a_playlist_source(tmp_path):
    for source in (-1, None, 0):
        (tmp_path / "sp-program.json").unlink(missing_ok=True)
        rc, out, err, _ = _run(
            tmp_path, 'if cg_chain_program_readback "$HOST" "$PY"; then echo MATCH; else echo NO; fi',
            sp_program=("sp-fast", source))
        assert rc == 0, err
        assert out.strip().endswith("NO"), (source, out, err)
    rc, out, err, _ = _run(
        tmp_path, 'if cg_chain_program_readback "$HOST" "$PY"; then echo MATCH; fi',
        sp_program=("sp-fast", 12))
    assert "MATCH" in out, out + err


def test_a_restore_press_that_does_not_read_back_keeps_the_snapshot(tmp_path):
    # SongPlayer answers OK for a press it kept (session.rs maps Switched::Kept to Reply::ok).
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": "sp-slow", "source": 3}))
    rc, out, err, calls = _run(tmp_path, 'cg_chain_sp_program_restore "$PY" 30; echo REACHED',
                               env={"FAKE_FACADE_NOOP": "1"}, sp_program=("sp-fast", 7))
    assert rc == 0 and "REACHED" in out
    assert any(c.startswith("cg_chain_scene.py facade-program") for c in calls)
    assert "did not read back" in err and "SongPlayer program restored" not in out, out + err
    assert (tmp_path / "cg-chain-sp-program-state.json").exists()
    assert not (tmp_path / "cg-chain-sp-program-state.json.restored").exists()


def test_a_snapshot_that_cannot_be_retired_is_loud_never_fatal(tmp_path):
    (tmp_path / "cg-chain-sp-program-state.json").write_text(
        json.dumps({"host": HOST, "port": 4456, "scene": "sp-slow", "source": 3}))
    locked = tmp_path / "locked"
    locked.mkdir()
    (locked / "cg-chain-sp-program-state.json").write_text(
        (tmp_path / "cg-chain-sp-program-state.json").read_text())
    locked.chmod(0o555)
    try:
        rc, out, err, _ = _run(
            tmp_path, 'cg_chain_sp_program_restore "$PY" 30; echo REACHED',
            env={"CG_CHAIN_STATE_DIR": str(locked)}, sp_program=("sp-slow", 3))
    finally:
        locked.chmod(0o755)
    assert rc == 0 and "REACHED" in out, "the restore promises rc 0 under the caller's set -e"
    assert "could not retire" in err, err


def test_uint_knobs_refuse_an_overlong_value(tmp_path):
    rc, out, err, _ = _run(
        tmp_path, 'CG_CHAIN_BURN_READBACK_S=99999999999999999999 cg_chain_burn_readback_secs; echo; '
                  '_cg_chain_uint_or 1234567 9; echo; _cg_chain_uint_or 123456 9')
    assert rc == 0, err
    assert out.split() == ["3", "9", "123456"], "more than 6 digits falls back to the default"


def test_rig_mode_event_survives_a_failing_cg_obs_sweep_and_the_contract_carries_it(tmp_path):
    # The traveling box must never abort the owner's pre-broadcast switch: its sweep-off failure is
    # a loud WARNING, and the contract's fail-closed sweep-check sentinel carries the verdict.
    out, err, log = _rig(tmp_path, 'toggle_burn event; echo "TOGGLE_RC=$?"', "resolume",
                         {"FAKE_PY_FAIL_HOST": "resolume.lan", "FAKE_PY_FAIL_RC": "2"})
    assert "TOGGLE_RC=0" in out, out + err
    assert "sweep-off --host resolume.lan" in log
    assert "WARNING" in err and "could not enumerate the cg OBS inputs on resolume.lan" in err, err


def test_rig_mode_event_still_fails_on_a_strih_sweep_failure(tmp_path):
    out, err, log = _rig(tmp_path, 'toggle_burn event; echo "TOGGLE_RC=$?"', "none",
                         {"FAKE_PY_FAIL_HOST": "10.77.9.202", "FAKE_PY_FAIL_RC": "1"})
    assert "TOGGLE_RC=0" not in out, "a strih/stream sweep failure still fails the switch"


def test_rig_mode_passes_its_obs_ws_password_to_the_cg_obs_sweep(tmp_path):
    out, err, log = _rig(tmp_path, "toggle_burn event", "resolume", {"OBS_WS_PASSWORD": "rigpw"})
    assert "sweep-off --host resolume.lan --password rigpw" in log, log


# ---- (c) the home-gated cg OBS burn backstop --------------------------------------------------


def test_backstop_target_is_the_fleet_host_only_while_home(tmp_path):
    rc, out, err, _ = _run(tmp_path, "OBS_FLEET_HOME=resolume cg_chain_backstop_sweep_targets")
    assert rc == 0, err
    assert out == "resolume.lan|-|resolume\n", "the traveling box by its fleet HOSTNAME, never a pinned IP"
    rc, out, err, _ = _run(tmp_path, "OBS_FLEET_HOME=strih-lx cg_chain_backstop_sweep_targets")
    assert rc == 0 and out == ""
    assert "[resolume burn-sweep] SKIP" in err and "away" in err, err


def test_backstop_sweep_off_sweeps_the_cg_obs_when_home(tmp_path):
    rc, out, err, calls = _run(tmp_path, 'OBS_FLEET_HOME=resolume cg_chain_backstop_sweep_off "$BF" 7')
    assert rc == 0, err
    assert calls == ["obs_burn_filter.py sweep-off --host resolume.lan"], calls
    assert "[resolume burn-sweep] [sweep] host=resolume.lan" in out, out
    assert (tmp_path / "timeout.log").read_text().split() == ["7"]
    assert "WARNING" not in err


def test_backstop_sweep_off_is_a_skip_when_away(tmp_path):
    rc, out, err, calls = _run(tmp_path, 'cg_chain_backstop_sweep_off "$BF" 7; echo REACHED')
    assert rc == 0 and "REACHED" in out
    assert calls == [] and "SKIP" in err


def test_backstop_enumeration_failure_is_loud_and_unverified_never_fatal(tmp_path):
    rc, out, err, calls = _run(
        tmp_path, 'OBS_FLEET_HOME=resolume cg_chain_backstop_sweep_off "$BF" 7; echo REACHED',
        env={"FAKE_SWEEP_RC": "2"})
    assert rc == 0 and "REACHED" in out, "the camera-chain E2E never aborts on the cg leg"
    assert "could not enumerate the cg OBS inputs on resolume.lan" in err and "UNVERIFIED" in err, err
    assert "obs_burn_filter.py sweep-off --host resolume.lan" in err


def test_backstop_other_failure_names_the_rc(tmp_path):
    rc, out, err, _ = _run(
        tmp_path, 'OBS_FLEET_HOME=resolume cg_chain_backstop_sweep_off "$BF" 7',
        env={"FAKE_SWEEP_RC": "1"})
    assert rc == 0
    assert "sweep on resolume.lan failed (rc=1)" in err, err


def test_backstop_passes_the_cg_obs_password_only_when_set(tmp_path):
    rc, _, err, calls = _run(
        tmp_path, 'OBS_FLEET_HOME=resolume CG_CHAIN_OBS_PASSWORD=s3 cg_chain_backstop_sweep_off "$BF" 7')
    assert rc == 0, err
    assert calls == ["obs_burn_filter.py sweep-off --host resolume.lan --password s3"]


# ---- rig-mode EVENT: the sweep-off and the contract's sweep-check cover the cg OBS ------------

_RIG_FAKE_PY = r'''#!/usr/bin/env bash
echo "PYCALL: $*" >> "$PYLOG"
echo ok
case " $* " in
  *" --host ${FAKE_PY_FAIL_HOST:-none} "*) exit "${FAKE_PY_FAIL_RC:-1}" ;;
esac
'''


def _rig(tmp_path, body, home, extra_env=None):
    fbin = tmp_path / "rbin"
    fbin.mkdir(exist_ok=True)
    py = fbin / "python3"
    py.write_text(_RIG_FAKE_PY)
    py.chmod(0o755)
    ack = tmp_path / "rig-fleet.txt"
    ack.write_text("# empty\n")
    pylog = tmp_path / "py.log"
    pylog.touch()
    harness = f'set -uo pipefail\n. "{_RIG}"\nset +e\n{body}\n'
    out = subprocess.run(
        ["/bin/bash", "-c", harness], capture_output=True, text=True, check=False,
        env={"PATH": f"{fbin}:/usr/local/bin:/usr/bin:/bin", "HOME": str(tmp_path),
             "PYLOG": str(pylog), "RIG_FLEET_ACK_FILE": str(ack), "OBS_FLEET_HOME": home,
             "IMAG_OFFLINE_ACKED": "1", "IMAG_OFFLINE_ACK_REASON": "test", **(extra_env or {})})
    return out.stdout, out.stderr, pylog.read_text()


def test_rig_mode_event_sweeps_the_cg_obs_when_home(tmp_path):
    out, err, log = _rig(tmp_path, "toggle_burn event", "resolume")
    assert "sweep-off --host resolume.lan" in log, log + err
    assert "[resolume burn-sweep] ok" in out, out


def test_rig_mode_event_skips_the_cg_obs_when_away(tmp_path):
    out, err, log = _rig(tmp_path, "toggle_burn event", "none")
    assert "resolume" not in log, log
    assert "[resolume burn-sweep] SKIP" in err, err


def test_rig_mode_test_never_touches_the_cg_obs(tmp_path):
    out, err, log = _rig(tmp_path, "toggle_burn test", "resolume")
    assert "resolume" not in log, "TEST (burn ON) stays pinned-only: the backstop is an OFF sweep"


def test_rig_mode_routes_both_event_sweeps_and_only_them_through_the_backstop():
    s = _RIG.read_text()
    assert s.count("done < <(obs_burn_targets; cg_chain_backstop_sweep_targets)") == 1, (
        "only the contract's fail-closed sweep-check loop reads the cg OBS row")
    assert s.count("done < <(obs_burn_targets)\n") == 3, (
        "the two pinned program-input loops and the EVENT sweep-off loop stay on the rig boxes")
    tb = s[s.index("toggle_burn() {"):]
    tb = tb[:tb.index("\n}\n")]
    assert 'cg_chain_backstop_sweep_off "$here/obs_burn_filter.py"' in tb, (
        "the EVENT sweep-off of the cg OBS is the backstop lib's WARN-only sweep")
    assert '. "$RIG_MODE_DIR/lib/cg-obs-burn-backstop.sh"' in s, "rig-mode sources the backstop lib"
    assert "cg-chain-e2e.sh" not in s, "rig-mode never pulls in the E2E-only CG_CHAIN profile lib"
    body = s[s.index("event_mode_assert() {"):s.index("\ndo_event() {")]
    check = body.index('obs_burn_filter.py" sweep-check --host "$_asbip"')
    loop_end = body.index("done < <(", check)
    assert body[loop_end:].startswith("done < <(obs_burn_targets; cg_chain_backstop_sweep_targets)"), (
        "the contract's fail-closed sweep-check reads the cg OBS row too")


# ---- recording-e2e.sh wiring (static reads) ---------------------------------------------------


def test_recording_e2e_runs_the_backstop_right_after_the_prerun_sweep():
    s = _E2E.read_text()
    loop = s.index('| sed "s/^/    [normalize sweep] /" || true\n  done\n')
    call = s.index('cg_chain_backstop_sweep_off "$HERE/obs_burn_filter.py" "$OBS_CLEANUP_TIMEOUT"')
    assert s.count("cg_chain_backstop_sweep_off") == 1
    assert loop < call < s.index("\n  # issue 1271: the read-only stray recording/streaming check", loop)


def test_recording_e2e_turns_the_songplayer_burn_off_only_when_one_is_owed():
    s = _E2E.read_text()
    assert ('if [ "$CG_RECORDING_STARTED" != 1 ] && [ "${CG_SP_BURN_OWED:-0}" = 1 ]; then '
            'cg_chain_songplayer_burn off; fi') in s


def test_the_songplayer_burn_goes_on_inside_the_record_start_after_the_cuts():
    s = _E2E.read_text()
    block = s[s.index("if cg_chain_enabled; then\n  echo \"[5/8] #1301"):s.index("# [5b/8] #707 B1")]
    assert "cg_chain_songplayer_burn on" not in block, (
        "a burn turned on before the program cut is not a burn on the cg recording")
    lib = _LIB.read_text()
    start = lib[lib.index("cg_chain_record_start() {"):]
    start = start[:start.index("\n}\n")]
    assert start.index("cg_chain_program_readback") < start.index("cg_chain_songplayer_burn on") \
        < start.index("cg_chain_cg_burn on") < start.index("--action start")
