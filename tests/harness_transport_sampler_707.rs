//! #707 B1 (freeze+jump discriminator, SECOND prong) — regression + behavior guard for
//! `scripts/lib/transport-sampler.sh` and its wiring into `scripts/recording-e2e.sh`.
//!
//! The #707 residual is a genuine ~2.7s NDI delivery interruption (strih shows frozen frames then a
//! +170-id JUMP) whose frames NEVER arrive at strih, while every BOX-side instrument reads clean —
//! so the final layer (link vs box-emit vs NDI SDK) was unnamed. Prong 1 is the box-side
//! `emit_rate_ring` 1s WARN (src/emit_rate_ring.rs). This prong records, on each cambox, the
//! box->strih TCP Send-Q + retransmit totals and the egress NIC error/drop counters every ~250ms
//! during the recording window, so the NEXT freeze's discriminator can be READ from the harvested
//! CSVs, not guessed.
//!
//! These tests exercise the shared lib DIRECTLY (source it, run the pure REMOTE-COMMAND-STRING
//! builders) — mirroring tests/harness_ndi_alive_lib.rs — plus static-anchor assertions that the
//! sampler is sourced + launched in [5b/8] (inside the recording window) + stopped/harvested in
//! [7c/8] with the CSVs landing in the run dir.

use std::path::PathBuf;
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Source the lib and run `body` in bash, returning stdout. Asserts the harness exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", repo("scripts/lib/transport-sampler.sh"))
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---- lib pure functions --------------------------------------------------------------------------

/// The CSV column contract is the single source of truth (loop body writes it; the analysis reads
/// it). Pin it exactly so a silent column reorder/rename can't desync the harvested data.
#[test]
fn csv_header_is_the_exact_eight_column_contract() {
    let out = run_sourced("transport_sampler_csv_header");
    assert_eq!(
        out.trim_end_matches('\n'),
        "epoch_ms,iface,send_q_bytes,retrans_total,rx_errors,rx_dropped,tx_errors,tx_dropped"
    );
}

/// Box IP -> stable short label for the harvested CSV filename (last octet .61..=.66 -> cam1..=cam6).
#[test]
fn box_label_maps_each_last_octet_to_its_cam_name() {
    assert_eq!(
        run_sourced("transport_sampler_box_label 10.77.9.61").trim(),
        "cam1"
    );
    assert_eq!(
        run_sourced("transport_sampler_box_label 10.77.9.62").trim(),
        "cam2"
    );
    assert_eq!(
        run_sourced("transport_sampler_box_label 10.77.9.63").trim(),
        "cam3"
    );
    assert_eq!(
        run_sourced("transport_sampler_box_label 10.77.9.64").trim(),
        "cam4"
    );
    assert_eq!(
        run_sourced("transport_sampler_box_label 10.77.9.65").trim(),
        "cam5"
    );
    assert_eq!(
        run_sourced("transport_sampler_box_label 10.77.9.66").trim(),
        "cam6"
    );
    // Unknown last octet must degrade to a stable, non-colliding label, never crash.
    assert_eq!(
        run_sourced("transport_sampler_box_label 10.77.9.99").trim(),
        "box-99"
    );
}

/// The loop body carries every signal the discriminator needs, self-terminates (ceiling + sentinel),
/// and paces at ~250ms — pinned so a future edit can't silently drop the retrans read or the stop.
#[test]
fn loop_body_samples_all_signals_paces_and_self_terminates() {
    let body = run_sourced("transport_sampler_loop_body");
    assert!(
        body.contains("epoch_ms,iface,send_q_bytes,retrans_total"),
        "loop body must write the CSV header first"
    );
    assert!(
        body.contains(r#"ss -tn "dst $TS_STRIH""#),
        "must sum the box->strih TCP Send-Q via `ss -tn dst <strih>`. Got:\n{body}"
    );
    assert!(
        body.contains(r#"ss -tin "dst $TS_STRIH""#) && body.contains("retrans:"),
        "must read per-connection retransmits via `ss -tin` + the retrans: field. Got:\n{body}"
    );
    assert!(
        body.contains("ip -s link show"),
        "must read the egress NIC error/drop counters via `ip -s link show <iface>`. Got:\n{body}"
    );
    assert!(
        body.contains("sleep 0.25"),
        "must pace at ~250ms per sample. Got:\n{body}"
    );
    assert!(
        body.contains("${TS_PID}.stop"),
        "must self-terminate on the stop sentinel so [7c/8]'s stop exits it cleanly. Got:\n{body}"
    );
    assert!(
        body.contains("${TS_MAX:-") && body.contains("date +%s"),
        "must self-terminate after a wall-clock ceiling so it can never orphan on a box. Got:\n{body}"
    );
}

/// The remote ARM command must be valid bash, export every TS_* param it needs, background the
/// loop, record the PID, and print the armed sentinel. The loop body is %q-quoted so the box's bash
/// re-parses it verbatim (awk single-quotes survive) — verify the generated command PARSES.
#[test]
fn remote_start_cmd_is_valid_bash_and_backgrounds_the_sampler() {
    let cmd = run_sourced(
        "transport_sampler_remote_start_cmd 10.77.9.202 250 /tmp/ts-707.csv 600 /tmp/ts-707.pid",
    );
    for needle in [
        "export TS_STRIH=",
        "TS_CSV=",
        "TS_MAX=",
        "TS_PID=",
        "nohup bash -c",
        "echo $! >",
        "transport-sampler-armed",
    ] {
        assert!(
            cmd.contains(needle),
            "arm cmd missing {needle:?}. Got:\n{cmd}"
        );
    }
    // The generated command must be syntactically valid bash (outer command; the %q-quoted body is
    // a single opaque arg to `bash -c`, so `bash -n` validates the wrapper we actually ssh-run).
    let syntax = Command::new("bash")
        .arg("-nc")
        .arg(&cmd)
        .output()
        .expect("bash -n");
    assert!(
        syntax.status.success(),
        "generated arm command is not valid bash:\n{cmd}\nstderr={}",
        String::from_utf8_lossy(&syntax.stderr)
    );
}

/// The remote STOP command drops the sentinel (clean loop exit) THEN hard-kills via the pidfile as a
/// backstop, and removes both files — valid bash, idempotent, never fails the caller.
#[test]
fn remote_stop_cmd_is_valid_bash_sentinel_then_kill() {
    let cmd = run_sourced("transport_sampler_remote_stop_cmd /tmp/ts-707.pid");
    assert!(
        cmd.contains(".stop"),
        "stop must drop the .stop sentinel. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("kill"),
        "stop must hard-kill via the pidfile as a backstop. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("rm -f"),
        "stop must clean up the pidfile + sentinel. Got:\n{cmd}"
    );
    let syntax = Command::new("bash")
        .arg("-nc")
        .arg(&cmd)
        .output()
        .expect("bash -n");
    assert!(
        syntax.status.success(),
        "generated stop command is not valid bash:\n{cmd}\nstderr={}",
        String::from_utf8_lossy(&syntax.stderr)
    );
}

/// End-to-end functional proof of the loop body itself: run it with a 0-second ceiling so it writes
/// the header, takes zero samples, and exits immediately (no lingering process). The CSV must exist
/// with exactly the contract header — proving the whole generated body is valid + writes the schema.
#[test]
fn loop_body_writes_the_header_and_exits_at_zero_ceiling() {
    let dir = std::env::temp_dir().join(format!("ts-707-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("out.csv");
    // TS_STRIH=127.0.0.1 → `ip route get 127.0.0.1` resolves locally (iface lo); TS_MAX=0 → the
    // while-loop condition is immediately false, so it writes only the header and returns.
    let harness = format!(
        "set -uo pipefail\n. \"$SCRIPT\"\nTS_STRIH=127.0.0.1 TS_CSV={csv} TS_MAX=0 \
         TS_PID={pid} bash -c \"$(transport_sampler_loop_body)\"",
        csv = csv.display(),
        pid = dir.join("nope.pid").display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", repo("scripts/lib/transport-sampler.sh"))
        .output()
        .expect("run loop body");
    assert!(
        out.status.success(),
        "loop body at zero ceiling must exit 0. stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got = std::fs::read_to_string(&csv).expect("loop body must have written the CSV");
    let lines: Vec<&str> = got.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("epoch_ms,iface,send_q_bytes,retrans_total,rx_errors,rx_dropped,tx_errors,tx_dropped"),
        "the written CSV's first line must be the contract header"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- wiring into recording-e2e.sh ---------------------------------------------------------------

/// The sampler lib must be SOURCED by the harness (so its functions are available at the call sites).
#[test]
fn recording_e2e_sources_the_transport_sampler_lib() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/transport-sampler.sh\""),
        "#707 B1: recording-e2e.sh must source scripts/lib/transport-sampler.sh"
    );
}

/// The sampler must be ARMED in a [5b/8] block AFTER the recording window opens (so it covers the
/// whole [5/8]->[7/8] window), using the pure builder — never an inline ssh loop.
#[test]
fn recording_e2e_arms_the_sampler_inside_the_recording_window() {
    let s = read("scripts/recording-e2e.sh");
    let start_pos = s
        .find("CAPTURE_RATE_WINDOW_START_EPOCH=")
        .expect("the recording-window START epoch anchor must exist");
    let arm_pos = s
        .find("[5b/8] transport sampler")
        .expect("#707 B1: a [5b/8] transport-sampler arm block must exist");
    assert!(
        arm_pos > start_pos,
        "#707 B1: the [5b/8] arm must run AFTER the recording window opens (StartRecord)"
    );
    assert!(
        s.contains("transport_sampler_remote_start_cmd \"$STRIH\""),
        "#707 B1: the arm must build its remote command via transport_sampler_remote_start_cmd (pure lib), not inline ssh"
    );
}

/// The sampler must be STOPPED + its CSV HARVESTED into the run dir in a [7c/8] block AFTER the
/// window END, and BEFORE the ~5-10 min decode step (so the CSVs are captured regardless of the
/// verdict outcome).
#[test]
fn recording_e2e_stops_and_harvests_the_sampler_after_the_window() {
    let s = read("scripts/recording-e2e.sh");
    let end_pos = s
        .find("CAPTURE_RATE_WINDOW_END_EPOCH=")
        .expect("the recording-window END epoch anchor must exist");
    let harvest_pos = s
        .find("[7c/8] transport sampler")
        .expect("#707 B1: a [7c/8] transport-sampler stop+harvest block must exist");
    assert!(
        harvest_pos > end_pos,
        "#707 B1: the [7c/8] stop+harvest must run AFTER the recording window closes (StopRecord)"
    );
    assert!(
        s.contains("transport_sampler_remote_stop_cmd \"$TS_REMOTE_PID\""),
        "#707 B1: the harvest must stop the sampler via transport_sampler_remote_stop_cmd (pure lib)"
    );
    // The CSV must land in the run dir ($OUTDIR) so it is harvested with the artifacts.
    let harvest_block = &s[harvest_pos..];
    assert!(
        harvest_block.contains("$OUTDIR/transport-sampler-")
            && harvest_block.contains("scp"),
        "#707 B1: the [7c/8] block must scp each per-box CSV into $OUTDIR/transport-sampler-<label>-<run>.csv"
    );
    // Must be harvested before the decode step spends its budget.
    let decode_pos = s
        .find("#193: by DEFAULT decode ON stream.lan")
        .expect("the #193 decode-on-stream branch comment must exist");
    assert!(
        harvest_pos < decode_pos,
        "#707 B1: the transport CSVs must be harvested BEFORE the decode step launches"
    );
}
