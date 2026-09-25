//! #1302 — the CG_CHAIN=1 E2E profile wired to the SHIPPED SongPlayer burn API, a default cg OBS
//! recording pull, and ONE tail CG window on strih (so strih + stream record the CG chain).
//!
//! All Tier-0 (no rig, no network): the pure builders are called directly, and the runners are
//! driven against FAKE `curl` / `sshpass` / `scp` / `python3` binaries put first on PATH inside the
//! bash snippet, so the exact request body, the health read-back and the scp source spec are
//! asserted without touching SongPlayer, OBS or resolume.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/cg-chain-e2e.sh")
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the lib under the caller's real `set -euo pipefail` and run `snippet`. Returns
/// (exit_ok, stdout, stderr).
fn run(snippet: &str) -> (bool, String, String) {
    let script = format!(
        "set -euo pipefail\n. \"{}\"\n{}",
        lib_script().display(),
        snippet
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// A health JSON shaped like the live `/api/v1/ndi/health` answer (read 25.9.2026): an array with
/// one object per SongPlayer output. `$1` = SP-fast's `burn_on`.
const HEALTH_FN: &str = r#"
health_json() {
  printf '[{"ndi_name":"SP-slow","burn_on":false,"lock_state":"LOCKED"},{"ndi_name":"SP-fast","burn_on":%s,"lock_state":"LOCKED"}]' "$1"
}
"#;

/// Fake `curl` on PATH: a GET of a URL ending in /api/v1/ndi/health prints $FAKE_HEALTH_FILE and
/// logs `GET m=<timeout>` to $FAKE_GETLOG; any other call (the burn POST) appends its URL, `-d`
/// body, `-X` method, `-H` header and `-m` timeout to $FAKE_LOG.
const FAKE_CURL: &str = r#"
FAKE_DIR="$(mktemp -d)"
trap 'rm -rf "$FAKE_DIR"' EXIT
export FAKE_LOG="$FAKE_DIR/curl.log" FAKE_HEALTH_FILE="$FAKE_DIR/health.json" FAKE_GETLOG="$FAKE_DIR/get.log"
cat > "$FAKE_DIR/curl" <<'SH'
#!/usr/bin/env bash
body=""; url=""; prev=""; method=""; hdr=""; tmo=""
for a in "$@"; do
  case "$prev" in
    -d|--data|--data-raw) body="$a" ;;
    -X) method="$a" ;;
    -H) hdr="$a" ;;
    -m) tmo="$a" ;;
  esac
  case "$a" in http*) url="$a" ;; esac
  prev="$a"
done
case "$url" in
  */api/v1/ndi/health) printf 'GET m=%s\n' "$tmo" >> "$FAKE_GETLOG"; cat "$FAKE_HEALTH_FILE" ;;
  *) printf 'POST %s %s METHOD=%s HDR=%s m=%s\n' "$url" "$body" "$method" "$hdr" "$tmo" >> "$FAKE_LOG" ;;
esac
SH
chmod +x "$FAKE_DIR/curl"
export PATH="$FAKE_DIR:$PATH"
export CG_CHAIN_BURN_RETRY_SLEEP=0
"#;

/// Fake `python3` on PATH that stands in for the two OBS-WS helpers and passes every other call to
/// the REAL python3 (the lib's JSON builders need it). Every helper call is logged to $PY_LOG:
///   - `cg_chain_scene.py strih-solo` prints $FAKE_SOLO_OUT (default `100<TAB>CG bridge`) and exits
///     $FAKE_SOLO_RC (default 0);
///   - `obs_phase2.py record --action stop` prints $FAKE_STOP_OUT;
///   - any other `cg_chain_scene.py` / `obs_phase2.py` call exits 0.
const FAKE_PY: &str = r#"
PY_DIR="$(mktemp -d)"
trap 'rm -rf "${FAKE_DIR:-/nonexistent-1302}" "$PY_DIR"' EXIT
REAL_PY="$(command -v python3)"
export PY_LOG="$PY_DIR/py.log" REAL_PY
cat > "$PY_DIR/python3" <<'SH'
#!/usr/bin/env bash
case "$*" in
  *cg_chain_scene.py*strih-solo*)
    printf '%s\n' "$*" >> "$PY_LOG"
    printf '%b\n' "${FAKE_SOLO_OUT:-100\tCG bridge}"
    exit "${FAKE_SOLO_RC:-0}" ;;
  *cg_chain_scene.py*|*obs_phase2.py*record*start*)
    printf '%s\n' "$*" >> "$PY_LOG"; exit 0 ;;
  *obs_phase2.py*record*stop*)
    printf '%s\n' "$*" >> "$PY_LOG"; printf '%s\n' "${FAKE_STOP_OUT:-}"; exit 0 ;;
  *) exec "$REAL_PY" "$@" ;;
esac
SH
chmod +x "$PY_DIR/python3"
export PATH="$PY_DIR:$PATH"
"#;

// ---- (a) the shipped burn API ------------------------------------------------------------------

#[test]
fn burn_url_is_the_shipped_api_with_an_env_base() {
    let (ok, def, _) = run("cg_chain_songplayer_burn_url");
    assert!(ok);
    assert_eq!(def, "http://resolume.lan:8920/api/v1/ndi/burn");
    let (_, ov, _) =
        run("CG_CHAIN_SONGPLAYER_API=http://10.77.9.201:8920/ cg_chain_songplayer_burn_url");
    assert_eq!(
        ov, "http://10.77.9.201:8920/api/v1/ndi/burn",
        "the base is env-configurable and a trailing slash never doubles"
    );
    let (_, h, _) = run("cg_chain_songplayer_health_url");
    assert_eq!(h, "http://resolume.lan:8920/api/v1/ndi/health");
}

#[test]
fn burn_body_carries_the_output_and_the_on_flag() {
    let (ok, on, _) = run("cg_chain_songplayer_burn_body on");
    assert!(ok);
    assert_eq!(on, r#"{"output":"SP-fast","on":true}"#);
    let (_, off, _) = run("cg_chain_songplayer_burn_body off");
    assert_eq!(off, r#"{"output":"SP-fast","on":false}"#);
    let (_, ov, _) = run("CG_CHAIN_SONGPLAYER_OUTPUT=SP-slow cg_chain_songplayer_burn_body on");
    assert_eq!(ov, r#"{"output":"SP-slow","on":true}"#);
    let (_, bad, _) =
        run("if cg_chain_songplayer_burn_body maybe; then echo BUILT; else echo REJECTED; fi");
    assert_eq!(
        bad, "REJECTED",
        "only on|off build a body — a typo never POSTs"
    );
}

#[test]
fn health_parse_reads_burn_on_of_the_named_output_only() {
    let snippet = format!(
        "{HEALTH_FN}\n\
         health_json true | cg_chain_health_burn_on SP-fast; echo\n\
         health_json false | cg_chain_health_burn_on SP-fast; echo\n\
         health_json true | cg_chain_health_burn_on SP-slow; echo\n\
         health_json true | cg_chain_health_burn_on SP-missing; echo\n\
         printf 'not json' | cg_chain_health_burn_on SP-fast; echo\n\
         printf '[{{\"ndi_name\":\"SP-fast\"}}]' | cg_chain_health_burn_on SP-fast; echo"
    );
    let (ok, out, _) = run(&snippet);
    assert!(ok, "the parser never fails the caller's set -e");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        ["true", "false", "false", "unknown", "unknown", "unknown"],
        "burn_on is read from the NAMED output; absent output / bad JSON / missing field = unknown"
    );
}

#[test]
fn burn_on_posts_the_body_and_verifies_it_on_health() {
    let snippet = format!(
        "{HEALTH_FN}{FAKE_CURL}\n\
         health_json true > \"$FAKE_HEALTH_FILE\"\n\
         cg_chain_songplayer_burn on\n\
         cat \"$FAKE_LOG\""
    );
    let (ok, out, _) = run(&snippet);
    assert!(ok);
    assert!(
        out.contains(
            r#"POST http://resolume.lan:8920/api/v1/ndi/burn {"output":"SP-fast","on":true}"#
        ),
        "the ON toggle POSTs the shipped body to the shipped URL: {out}"
    );
    assert!(
        out.contains("METHOD=POST HDR=Content-Type: application/json"),
        "the toggle is a POST with a JSON content type: {out}"
    );
    assert!(
        out.contains("VERIFIED"),
        "burn_on=true read back from /api/v1/ndi/health is reported as verified: {out}"
    );
}

#[test]
fn burn_off_that_never_reads_back_false_is_a_loud_leak_and_never_aborts() {
    let snippet = format!(
        "{HEALTH_FN}{FAKE_CURL}\n\
         health_json true > \"$FAKE_HEALTH_FILE\"\n\
         CG_CHAIN_BURN_ATTEMPTS=3 cg_chain_songplayer_burn off\n\
         echo REACHED\n\
         grep -c 'on\":false' \"$FAKE_LOG\""
    );
    let (ok, out, err) = run(&snippet);
    assert!(ok, "a leaked burn is loud but never aborts cleanup()");
    assert!(out.contains("REACHED"));
    assert!(
        out.ends_with('3'),
        "OFF is retried CG_CHAIN_BURN_ATTEMPTS times: {out}"
    );
    assert!(
        err.contains("LEAK"),
        "a burn that stays on after OFF is reported as a LEAK: {err}"
    );
}

#[test]
fn burn_off_verified_on_the_first_read_back_posts_once() {
    let snippet = format!(
        "{HEALTH_FN}{FAKE_CURL}\n\
         health_json false > \"$FAKE_HEALTH_FILE\"\n\
         cg_chain_songplayer_burn off\n\
         grep -c . \"$FAKE_LOG\""
    );
    let (ok, out, err) = run(&snippet);
    assert!(ok);
    assert!(out.contains("VERIFIED"), "{out}");
    assert!(out.ends_with('1'), "a verified OFF is not re-posted: {out}");
    assert!(!err.contains("LEAK"), "{err}");
}

// ---- (b) the default recording pull ------------------------------------------------------------

#[test]
fn pull_source_spec_uses_forward_slashes_and_keeps_spaces() {
    let (ok, spec, _) = run(
        r#"cg_chain_pull_source_spec newlevel 10.77.9.201 'C:\Users\Resolume\Videos\2026-09-25 07-24-00.mkv'"#,
    );
    assert!(ok);
    assert_eq!(
        spec,
        "newlevel@10.77.9.201:C:/Users/Resolume/Videos/2026-09-25 07-24-00.mkv"
    );
}

#[test]
fn record_stop_keeps_the_stoprecord_host_path() {
    // A fake python3 stands in for `obs_phase2.py record --action stop` (it prints the host path).
    let snippet = r#"
D="$(mktemp -d)"; trap 'rm -rf "$D"' EXIT
cat > "$D/python3" <<'SH'
#!/usr/bin/env bash
echo 'C:\Users\Resolume\Videos\2026-09-25 07-24-00.mkv'
SH
chmod +x "$D/python3"; PATH="$D:$PATH"
cg_chain_record_stop 10.77.9.201 /x/obs_phase2.py 5
printf 'PATH=%s\n' "$CG_HOST_RECORDING_PATH"
"#;
    let (ok, out, _) = run(snippet);
    assert!(ok);
    assert!(
        out.contains(r"PATH=C:\Users\Resolume\Videos\2026-09-25 07-24-00.mkv"),
        "{out}"
    );
}

#[test]
fn record_stop_with_an_empty_answer_keeps_the_earlier_path() {
    // The cleanup() re-stop of an already-stopped recording prints nothing; it must never clear the
    // path the first stop recorded.
    let snippet = format!(
        "{FAKE_PY}\n\
         CG_HOST_RECORDING_PATH='C:\\first.mkv'\n\
         FAKE_STOP_OUT='' cg_chain_record_stop 10.77.9.201 /x/obs_phase2.py 5\n\
         printf 'PATH=%s\\n' \"$CG_HOST_RECORDING_PATH\""
    );
    let (ok, out, _) = run(&snippet);
    assert!(ok);
    assert!(out.contains(r"PATH=C:\first.mkv"), "{out}");
}

#[test]
fn default_pull_scps_the_exact_stoprecord_file_to_the_local_dest() {
    let snippet = r#"
D="$(mktemp -d)"; trap 'rm -rf "$D"' EXIT
cat > "$D/sshpass" <<'SH'
#!/usr/bin/env bash
shift 2; exec "$@"
SH
cat > "$D/scp" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$SCP_LOG"
for last in "$@"; do :; done
echo cg-bytes > "$last"
SH
chmod +x "$D/sshpass" "$D/scp"; PATH="$D:$PATH"; export SCP_LOG="$D/scp.log"
unset CG_CHAIN_PULL_CMD
CG_HOST_RECORDING_PATH='C:\Users\Resolume\Videos\2026-09-25 07-24-00.mkv'
if cg_chain_pull_recording 10.77.9.201 "$D/cg.mkv"; then echo PULLED; fi
grep -c 'newlevel@10.77.9.201:C:/Users/Resolume/Videos/2026-09-25 07-24-00.mkv' "$SCP_LOG"
cat "$D/cg.mkv"
"#;
    let (ok, out, _) = run(snippet);
    assert!(ok);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.contains(&"PULLED"), "{out}");
    assert!(
        lines.contains(&"1"),
        "the scp source is the exact StopRecord file: {out}"
    );
    assert!(lines.contains(&"cg-bytes"), "{out}");
}

#[test]
fn a_failed_pull_leaves_no_partial_or_stale_file_for_the_cg_gate() {
    // recording-e2e.sh feeds --cg on `[ -f "$CG_RECORDING" ]`, so a failed scp must leave NO file at
    // the destination — not a partial one, and not a stale one from an earlier attempt.
    let snippet = r#"
D="$(mktemp -d)"; trap 'rm -rf "$D"' EXIT
cat > "$D/sshpass" <<'SH'
#!/usr/bin/env bash
shift 2; exec "$@"
SH
cat > "$D/scp" <<'SH'
#!/usr/bin/env bash
for last in "$@"; do :; done
echo partial > "$last"
exit 1
SH
chmod +x "$D/sshpass" "$D/scp"; PATH="$D:$PATH"
unset CG_CHAIN_PULL_CMD
echo stale > "$D/cg.mkv"
CG_HOST_RECORDING_PATH='C:\x.mkv'
if cg_chain_pull_recording 10.77.9.201 "$D/cg.mkv"; then echo GOT; else echo NONE; fi
if [ -e "$D/cg.mkv" ]; then echo LEFTOVER; else echo CLEAN; fi
ls "$D" | grep -c 'cg.mkv' || true
"#;
    let (ok, out, _) = run(snippet);
    assert!(ok);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.contains(&"NONE"), "{out}");
    assert!(lines.contains(&"CLEAN"), "no cg file may remain: {out}");
    assert_eq!(lines.last(), Some(&"0"), "no .part file either: {out}");
}

#[test]
fn a_failed_operator_pull_cmd_leaves_no_file_for_the_cg_gate() {
    // The CG_CHAIN_PULL_CMD override gets the same guarantee: a command that fails after writing
    // part of the file must not leave it behind for the merge's `[ -f "$CG_RECORDING" ]` gate.
    let snippet = r#"
D="$(mktemp -d)"; trap 'rm -rf "$D"' EXIT
CG_CHAIN_PULL_CMD='echo partial > "$CG_RECORDING"; exit 1'
if cg_chain_pull_recording 10.77.9.201 "$D/cg.mkv"; then echo GOT; else echo NONE; fi
if [ -e "$D/cg.mkv" ]; then echo LEFTOVER; else echo CLEAN; fi
"#;
    let (ok, out, _) = run(snippet);
    assert!(ok);
    assert_eq!(out.lines().collect::<Vec<_>>(), ["NONE", "CLEAN"], "{out}");
}

#[test]
fn default_pull_without_a_host_path_omits_cg() {
    let (ok, out, _) = run("unset CG_CHAIN_PULL_CMD CG_HOST_RECORDING_PATH; \
         if cg_chain_pull_recording 1.2.3.4 /tmp/none-1302.mkv; then echo GOT; else echo NONE; fi");
    assert!(ok);
    assert_eq!(out.lines().last(), Some("NONE"));
}

// ---- (c) the ONE tail CG window ----------------------------------------------------------------

#[test]
fn window_due_needs_the_profile_and_a_started_cg_recording() {
    let (_, a, _) = run(
        "unset CG_CHAIN; CG_RECORDING_STARTED=1; if cg_chain_window_due; then echo Y; else echo N; fi",
    );
    assert_eq!(a, "N", "CG_CHAIN unset = inert");
    let (_, b, _) = run(
        "CG_CHAIN=1 CG_RECORDING_STARTED=0; if cg_chain_window_due; then echo Y; else echo N; fi",
    );
    assert_eq!(b, "N", "no cg recording = nothing to judge, no strih cut");
    let (_, c, _) = run(
        "CG_CHAIN=1 CG_RECORDING_STARTED=1; if cg_chain_window_due; then echo Y; else echo N; fi",
    );
    assert_eq!(c, "Y");
}

#[test]
fn window_secs_defaults_and_rejects_garbage() {
    let (_, d, _) = run("unset CG_CHAIN_WINDOW_SECS; cg_chain_window_secs");
    assert_eq!(d, "30");
    let (_, o, _) = run("CG_CHAIN_WINDOW_SECS=45 cg_chain_window_secs");
    assert_eq!(o, "45");
    let (_, g, _) = run("CG_CHAIN_WINDOW_SECS=0 cg_chain_window_secs");
    assert_eq!(
        g, "30",
        "a non-positive / non-integer value falls back to the default"
    );
}

#[test]
fn window_json_is_the_one_cg_window_record() {
    let (ok, j, _) = run("cg_chain_window_json 'CG bridge' CG-obs 100 250");
    assert!(ok);
    assert_eq!(
        j,
        r#"{"kind":"cg","scene":"CG bridge","input":"CG-obs","start_ns":100,"end_ns":250}"#
    );
    let (_, bad, _) = run(
        "if cg_chain_window_json 'CG bridge' CG-obs 250 100; then echo BUILT; else echo REJECTED; fi",
    );
    assert_eq!(bad, "REJECTED", "a window must have start_ns < end_ns");
}

#[test]
fn window_record_is_keyed_to_the_run() {
    let (_, a, _) = run("unset RUN_ID; CG_CHAIN_STATE_DIR=/r cg_chain_window_file");
    assert_eq!(a, "/r/cg-window.json");
    let (_, b, _) = run("RUN_ID=77 CG_CHAIN_STATE_DIR=/r cg_chain_window_file");
    assert_eq!(b, "/r/cg-window-77.json");
}

#[test]
fn window_cut_timeout_covers_the_non_black_check() {
    // The strih cut enumerates every scene AND runs the polled non-black check
    // (OBS_BLACKCHECK_TIMEOUT_S, default 20 s), so it gets that budget + 30 s, never less than the
    // caller's timeout.
    let (_, d, _) = run("unset OBS_BLACKCHECK_TIMEOUT_S; cg_chain_window_cut_timeout 30");
    assert_eq!(d, "50");
    let (_, e, _) = run("OBS_BLACKCHECK_TIMEOUT_S=40 cg_chain_window_cut_timeout 30");
    assert_eq!(e, "70");
    let (_, f, _) = run("unset OBS_BLACKCHECK_TIMEOUT_S; cg_chain_window_cut_timeout 100");
    assert_eq!(f, "100");
    let (_, g, _) = run("OBS_BLACKCHECK_TIMEOUT_S=abc cg_chain_window_cut_timeout 30");
    assert_eq!(g, "50", "a non-integer check budget falls back to 20 s");
}

#[test]
fn state_files_live_in_the_run_dir() {
    let (_, s, _) = run("unset RUN_ID; CG_CHAIN_STATE_DIR=/r/out cg_chain_state_file strih-scene");
    assert_eq!(s, "/r/out/cg-chain-strih-scene-state.json");
    let (_, r, _) = run("RUN_ID=4711 CG_CHAIN_STATE_DIR=/r/out cg_chain_state_file cg-program");
    assert_eq!(
        r, "/r/out/cg-chain-cg-program-state-4711.json",
        "a snapshot is keyed to its run, so a reused OUTDIR never replays another run's snapshot"
    );
    let (_, p, _) = run("cg_chain_scene_py /x/scripts/obs_phase2.py");
    assert_eq!(p, "/x/scripts/cg_chain_scene.py");
    let (_, c, _) = run("unset CG_CHAIN_CG_SCENE; cg_chain_cg_scene");
    assert_eq!(
        c, "sp-fast",
        "the cg OBS scene defaults to the lower-cased output name"
    );
}

#[test]
fn disabled_profile_never_calls_obs_or_songplayer() {
    // CG_CHAIN unset: the window, the after-StopRecord step and cleanup are byte-inert (no python3,
    // no curl).
    let snippet = r#"
D="$(mktemp -d)"; trap 'rm -rf "$D"' EXIT
for b in python3 curl scp sshpass; do
  printf '#!/usr/bin/env bash\necho %s >> "%s/called"\n' "$b" "$D" > "$D/$b"; chmod +x "$D/$b"
done
PATH="$D:$PATH"; unset CG_CHAIN
cg_chain_window 10.77.9.202 /x/obs_phase2.py 5
cg_chain_after_stoprecord 10.77.9.201 /x/obs_phase2.py 5
cg_chain_cleanup "" /x/obs_phase2.py 5
if [ -f "$D/called" ]; then cat "$D/called"; else echo INERT; fi
"#;
    let (ok, out, _) = run(snippet);
    assert!(ok);
    assert_eq!(out, "INERT");
}

#[test]
fn window_cuts_strih_holds_it_and_writes_the_window_record() {
    let snippet = format!(
        "{FAKE_PY}\n\
         OUT=\"$(mktemp -d)\"; unset RUN_ID\n\
         CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_CHAIN_STATE_DIR=\"$OUT\" CG_CHAIN_WINDOW_SECS=1 \\\n\
           cg_chain_window 10.77.9.202 /x/obs_phase2.py 5\n\
         cat \"$OUT/cg-window.json\"; echo\n\
         cat \"$PY_LOG\"\n\
         rm -rf \"$OUT\""
    );
    let (ok, out, err) = run(&snippet);
    assert!(ok, "{err}");
    assert!(
        out.contains(
            r#"{"kind":"cg","scene":"CG bridge","input":"CG-obs","start_ns":100,"end_ns":"#
        ),
        "the window record carries the helper's cut instant + scene: {out}"
    );
    assert!(
        out.contains("strih-solo --host 10.77.9.202 --input CG-obs"),
        "the helper cuts strih to the CG-obs scene: {out}"
    );
}

#[test]
fn window_with_a_failing_cut_returns_zero_and_writes_nothing() {
    let snippet = format!(
        "{FAKE_PY}\n\
         OUT=\"$(mktemp -d)\"; unset RUN_ID\n\
         CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_CHAIN_STATE_DIR=\"$OUT\" CG_CHAIN_WINDOW_SECS=1 \\\n\
           FAKE_SOLO_RC=2 cg_chain_window 10.77.9.202 /x/obs_phase2.py 5\n\
         echo REACHED\n\
         if [ -f \"$OUT/cg-window.json\" ]; then echo WROTE; else echo NOFILE; fi\n\
         rm -rf \"$OUT\""
    );
    let (ok, out, err) = run(&snippet);
    assert!(ok, "a failed CG cut never aborts the run: {err}");
    assert!(out.contains("REACHED") && out.contains("NOFILE"), "{out}");
    assert!(err.contains("strih CG cut failed"), "{err}");
}

#[test]
fn record_start_cuts_cg_program_before_start_record() {
    let snippet = format!(
        "{FAKE_PY}\n\
         OUT=\"$(mktemp -d)\"\n\
         if CG_CHAIN_STATE_DIR=\"$OUT\" cg_chain_record_start 10.77.9.201 /x/obs_phase2.py 5; then echo STARTED; fi\n\
         cat \"$PY_LOG\"\n\
         rm -rf \"$OUT\""
    );
    let (ok, out, _) = run(&snippet);
    assert!(ok);
    let program = out
        .find("cg_chain_scene.py program --host 10.77.9.201 --scene sp-fast")
        .expect("the cg program cut to the SongPlayer scene");
    let start = out
        .find("obs_phase2.py record --host 10.77.9.201 --action start")
        .expect("the cg StartRecord");
    assert!(
        program < start,
        "cg program must show SP-fast BEFORE the recording starts: {out}"
    );
    assert!(out.contains("STARTED"), "{out}");
}

#[test]
fn after_stoprecord_stops_cg_turns_the_burn_off_and_restores_strih() {
    let snippet = format!(
        "{HEALTH_FN}{FAKE_CURL}{FAKE_PY}\n\
         health_json false > \"$FAKE_HEALTH_FILE\"\n\
         OUT=\"$(mktemp -d)\"; echo '{{}}' > \"$OUT/cg-chain-strih-scene-state.json\"\n\
         echo '{{}}' > \"$OUT/cg-chain-cg-program-state.json\"\n\
         unset RUN_ID\n\
         CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_CHAIN_STATE_DIR=\"$OUT\" FAKE_STOP_OUT='C:\\cg.mkv' \\\n\
           cg_chain_after_stoprecord 10.77.9.201 /x/obs_phase2.py 5\n\
         printf 'PATH=%s\\n' \"${{CG_HOST_RECORDING_PATH:-}}\"\n\
         cat \"$PY_LOG\"; cat \"$FAKE_LOG\"\n\
         rm -rf \"$OUT\""
    );
    let (ok, out, err) = run(&snippet);
    assert!(ok, "{err}");
    assert!(
        out.contains("obs_phase2.py record --host 10.77.9.201 --action stop"),
        "{out}"
    );
    assert!(
        out.contains(r"PATH=C:\cg.mkv"),
        "the stop keeps the host path: {out}"
    );
    assert!(
        out.contains(r#"{"output":"SP-fast","on":false}"#),
        "the burn goes OFF right after the recordings stop: {out}"
    );
    assert!(
        out.contains("cg-chain-strih-scene-state.json")
            && out.contains("cg-chain-cg-program-state.json"),
        "strih AND the cg program are restored right after StopRecord: {out}"
    );
}

#[test]
fn cleanup_never_stops_a_cg_recording_this_run_did_not_start() {
    // CG_HOST_IP is set as soon as the host resolves — BEFORE StartRecord — so cleanup() must key the
    // cg StopRecord on CG_RECORDING_STARTED, never on the host alone (never stop a recording this run
    // did not start).
    let snippet = format!(
        "{HEALTH_FN}{FAKE_CURL}{FAKE_PY}\n\
         health_json false > \"$FAKE_HEALTH_FILE\"\n\
         touch \"$PY_LOG\"\n\
         CG_CHAIN=1 CG_RECORDING_STARTED=0 CG_CHAIN_STATE_DIR=\"$(mktemp -d)\" \\\n\
           cg_chain_cleanup 10.77.9.201 /x/obs_phase2.py 5\n\
         if grep -q 'record --host 10.77.9.201 --action stop' \"$PY_LOG\"; then echo STOPPED; else echo UNTOUCHED; fi"
    );
    let (ok, out, err) = run(&snippet);
    assert!(ok, "{err}");
    assert!(out.ends_with("UNTOUCHED"), "{out}");
}

#[test]
fn cleanup_turns_the_burn_off_fast_and_restores_both_snapshots() {
    let snippet = format!(
        "{HEALTH_FN}{FAKE_CURL}{FAKE_PY}\n\
         health_json false > \"$FAKE_HEALTH_FILE\"\n\
         OUT=\"$(mktemp -d)\"\n\
         echo '{{}}' > \"$OUT/cg-chain-strih-scene-state-9.json\"\n\
         echo '{{}}' > \"$OUT/cg-chain-cg-program-state-9.json\"\n\
         RUN_ID=9 CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_CHAIN_STATE_DIR=\"$OUT\" \\\n\
           cg_chain_cleanup 10.77.9.201 /x/obs_phase2.py 5\n\
         cat \"$PY_LOG\"; cat \"$FAKE_LOG\"; cat \"$FAKE_GETLOG\"\n\
         rm -rf \"$OUT\""
    );
    let (ok, out, err) = run(&snippet);
    assert!(ok, "{err}");
    assert!(
        out.contains(r#"{"output":"SP-fast","on":false}"#),
        "cleanup turns the burn OFF: {out}"
    );
    assert!(
        out.contains("cg-chain-strih-scene-state-9.json")
            && out.contains("cg-chain-cg-program-state-9.json"),
        "cleanup restores BOTH snapshots of THIS run: {out}"
    );
    assert!(
        out.contains("m=3") && !out.contains("m=10"),
        "cleanup uses the short per-request burn timeout so it never stalls the teardowns after it: {out}"
    );
}

// ---- recording-e2e.sh wiring (static reads) ----------------------------------------------------

#[test]
fn recording_e2e_runs_the_cg_window_after_the_camera_schedule_and_before_stoprecord() {
    let s = recording_e2e_text();
    let window = s
        .find("if cg_chain_window_due; then cg_chain_window \"$STRIH\"")
        .expect("#1302: the tail CG window must be wired, gated by cg_chain_window_due");
    let schedule = s
        .find("echo \"    wrote switch schedule -> $SWITCH_SCHEDULE_JSON\"")
        .expect("the ALL_CAMBOX schedule write");
    let steady = s
        .find("interruptible_sleep \"$(( DURATION + RECORD_PAD ))\"")
        .expect("the steady-state hold");
    let stop = s
        .find("echo \"[7/8] StopRecord + download strih + stream recordings to dev1")
        .expect("the [7/8] banner");
    assert!(
        schedule < window && steady < window,
        "the CG window is a TAIL window: after every camera window, so the camera verdict never sees it"
    );
    assert!(
        window < stop,
        "the CG window must be recorded (before StopRecord)"
    );
}

#[test]
fn recording_e2e_ends_the_cg_leg_right_after_stoprecord() {
    let s = recording_e2e_text();
    let stop = s
        .find("STREAM_HOST_PATH=$(python3 \"$HERE/obs_phase2.py\" record --host \"$STREAM\" --action stop)")
        .expect("the [7/8] stream StopRecord");
    let after = s
        .find("cg_chain_after_stoprecord \"${CG_HOST_IP:-}\" \"$HERE/obs_phase2.py\"")
        .expect("#1302: the cg leg (cg StopRecord, burn OFF, strih restore) ends after [7/8]");
    let pull = s
        .find("cg_chain_pull_recording \"$CG_HOST_IP\" \"$CG_RECORDING\"")
        .expect("the [8/8d] pull");
    assert!(stop < after && after < pull);
    // It must not skew the genlock-audit AFTER snapshot (its window spans EXACTLY the recording) nor
    // run ahead of the post-record stomp re-check.
    let audit = s
        .find("genlock_audit_snapshot_capture after \"$OUTDIR/genlock-audit-after-${RUN_ID}.txt\"")
        .expect("the genlock-audit AFTER snapshot");
    let stomp = s
        .find("measurement_eq_post_record_stomp_recheck \"$MEASUREMENT_EQ_PROFILE\"")
        .expect("the post-record stomp re-check");
    assert!(
        audit < after && stomp < after,
        "the CG leg ends AFTER both reads"
    );
}

#[test]
fn recording_e2e_turns_the_burn_off_when_the_cg_recording_never_started() {
    let s = recording_e2e_text();
    let start = s.find("cg_chain_record_start").expect("the cg StartRecord");
    let off = s
        .find("if [ \"$CG_RECORDING_STARTED\" != 1 ]; then cg_chain_songplayer_burn off; fi")
        .expect("#1302: a burn with no cg recording is turned straight back off");
    let next = s
        .find("# [5b/8] #707 B1")
        .expect("the [5b/8] step banner comment");
    assert!(start < off && off < next);
}

#[test]
fn recording_e2e_points_the_state_dir_at_the_run_dir() {
    let s = recording_e2e_text();
    assert!(
        s.contains("CG_CHAIN_STATE_DIR=\"$OUTDIR\""),
        "#1302: the cg/strih restore snapshots live in this run's OUTDIR"
    );
}
