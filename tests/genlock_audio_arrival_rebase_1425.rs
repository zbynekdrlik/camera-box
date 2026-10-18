//! Issue 1425 (E3 slice S4, design 6225701358) — the ARRIVAL rebase through the shipped ingest: a
//! lift-and-compile of the place-vs-append branch of `source_output_audio_data`
//! (`vendor/obs-studio/libobs/obs-source.c`), the issue-1381 lift reused unchanged
//! (`tests/genlock_audio_ingest_lift/mod.rs`).
//!
//! A LOCKED arrival-basis source (an Off hold on non-direct stamps, or a latency hold; mixed, a
//! resampler, its arrival servo captured its depth) has its stamps on a clock other than the mixer's
//! (stock VLC's `libvlc_clock()`), and its continuation advances by the resampled frames, so the two
//! drift apart. OBS PLACED such a packet at its stamp once they were 70 ms apart (a 70 ms gap or
//! overwrite of its own audio), and past 2 s dropped the whole buffer and re-latched `timing_adjust`;
//! a 70 ms jump of the video-written `timing_adjust` placed it too. Once synced, nothing moves a source's
//! placement at runtime (owner, 16.10.2026): the decision (`src/genlock_audio_pairing.rs`
//! `audio_arrival_rebase`, its C port parity-gated by `tests/genlock_audio_asrc_basis_parity_1425.rs`)
//! rebases the timeline instead, so the packet APPENDS. Every other source keeps OBS's own path: an
//! unlocked one, a stopped one (the mixer cleared `audio_ts`), a listen-only one, one without a
//! resampler, a stamp-basis and a timecode source.
//!
//! The scenarios (`tests/c/genlock_audio_arrival_rebase_1425_scenarios.c`) replace the issue-1381
//! harness's `main`. The `genlock-audio-arrival-rebase` line each rebase prints is lifted too and
//! checked value for value, with its one-per-minute limit. It FAILS LOUDLY when no C compiler is
//! present.

mod genlock_audio_ingest_lift;
mod genlock_audio_pairing_lift;
use genlock_audio_ingest_lift::{compile_and_run, find_fn, ingest_harness, read, OBS_SOURCE};

const SCENARIOS: &str = "tests/c/genlock_audio_arrival_rebase_1425_scenarios.c";
const LOG_FN: &str = "static void genlock_audio_arrival_rebase_log(";
const LOG_DEFINE: &str = "#define GENLOCK_AUDIO_ARRIVAL_REBASE_LOG_NS ";

/// The shipped log helper with its rate-limit define. Before it shipped (the RED commit) a stand-in
/// that prints nothing, so this gate fails on BEHAVIOUR there; a helper deleted later fails the same way
/// (the truth table's log lines).
fn log_helper(src: &str) -> String {
    match find_fn(src, LOG_FN) {
        Some(f) => {
            let define = src
                .lines()
                .find(|l| l.starts_with(LOG_DEFINE))
                .expect("issue 1425: the log helper is shipped without its rate-limit define");
            format!("{define}\n{f}")
        }
        None => {
            "static void genlock_audio_arrival_rebase_log(obs_source_t *source, uint64_t now_ns)\n\
                 {\n\t(void)source;\n\t(void)now_ns;\n}\n"
                .to_string()
        }
    }
}

fn c_trace() -> Vec<String> {
    let harness = ingest_harness();
    let main = "int main(void)\n{";
    assert_eq!(
        harness.matches(main).count(),
        1,
        "issue 1425: the issue-1381 harness must define main exactly once"
    );
    let harness = harness.replace(main, "static int h_main_1381(void)\n{");
    let scenarios = read(SCENARIOS);
    assert_eq!(
        scenarios.matches("@ARRIVAL_LOG_FN@").count(),
        1,
        "issue 1425: {SCENARIOS} must carry @ARRIVAL_LOG_FN@ exactly once"
    );
    let scenarios = scenarios.replace("@ARRIVAL_LOG_FN@", &log_helper(&read(OBS_SOURCE)));
    compile_and_run(
        &format!("{harness}\n{scenarios}"),
        "genlock_audio_arrival_rebase_1425",
    )
}

/// One traced event packet: appended (the samples joined the buffer end), a timeline reset, a dropped
/// buffer, `timing_adjust` re-latched, the cumulative rebase count, the stock LOG_DEBUG lines it printed.
fn line(what: &str, f: [u8; 4], rebases: u32, debug: u8) -> String {
    let [appended, reset, dropped, ta_moved] = f;
    format!(
        "{what} appended={appended} reset={reset} dropped={dropped} ta_moved={ta_moved} \
         rebases={rebases} debug={debug}"
    )
}

fn log(src: &str, domain: &str, drift: &str, rebases: u32) -> String {
    format!(
        "log genlock-audio-arrival-rebase '{src}': domain={domain} drift_ms={drift} \
         rebases={rebases} (issue 1425)"
    )
}

const APPENDED: [u8; 4] = [1, 0, 0, 0];
const PLACED: [u8; 4] = [0, 0, 0, 0];

/// The decided behaviour, packet by packet.
fn truth_table() -> Vec<String> {
    let mut t = Vec::new();
    // a locked arrival source 70 ms off its continuation, either way: appended (OBS placed it, a
    // 70 ms gap or overwrite), no stock debug line, one rebase line with the drift
    for (name, drift) in [
        ("arrival_drift_70ms", "+70.0"),
        ("arrival_drift_back_70ms", "-70.0"),
    ] {
        t.push(format!("== {name}"));
        t.push(line("drift", APPENDED, 1, 0));
        t.push(log(name, "stamp", drift, 1));
        t.push(line("after", APPENDED, 1, 0));
    }
    // a latency hold is an arrival source too
    t.push("== arrival_latency_hold_70ms".to_string());
    t.push(line("drift", APPENDED, 1, 0));
    t.push(log("arrival_latency_hold_70ms", "stamp", "+70.0", 1));
    // a 3 s stamp jump with continuous arrival: appended, the buffer kept, timing_adjust untouched
    // (OBS: handle_ts_jump -- reset, the buffer dropped, timing_adjust re-latched, a "jumped" line)
    t.push("== arrival_jump_3s".to_string());
    t.push(line("jump", APPENDED, 1, 0));
    t.push(log("arrival_jump_3s", "stamp", "+3000.0", 1));
    t.push(line("after", APPENDED, 1, 0));
    // timing_adjust moved 80 ms (the video re-pairs an async source): the system domain rebases
    t.push("== arrival_sys_jump_80ms".to_string());
    t.push(line("sys_jump", APPENDED, 1, 0));
    t.push(log("arrival_sys_jump_80ms", "system", "+80.0", 1));
    t.push(line("after", APPENDED, 1, 0));
    // one line per minute per source: the second rebase (11 packets later) counts and prints nothing,
    // the third (60.4 s after the first line) prints again with the count
    t.push("== arrival_rate_limited".to_string());
    t.push(line("first", APPENDED, 1, 0));
    t.push(log("arrival_rate_limited", "stamp", "+70.0", 1));
    t.push(line("second", APPENDED, 2, 0));
    t.push(line("third", APPENDED, 3, 0));
    t.push(log("arrival_rate_limited", "stamp", "+70.0", 3));
    // OBS's own path: an unlocked arrival source is placed at its stamp ("exceeded" debug line)
    t.push("== arrival_unlocked".to_string());
    t.push(line("drift", PLACED, 0, 1));
    // a stopped source (audio_ts 0) resuming after a pause: placed by OBS as before, counts nothing (its
    // stamp domain still continues from the packet's own stamp, which is where OBS places it anyway)
    t.push("== arrival_stopped_500ms".to_string());
    t.push(line("resume", PLACED, 0, 0));
    // a listen-only source and one without a resampler: OBS's own path
    for name in ["arrival_listen_only", "arrival_no_resampler"] {
        t.push(format!("== {name}"));
        t.push(line("drift", PLACED, 0, 1));
    }
    // a stamp-basis (direct stamps) and a timecode source: OBS's own path
    t.push("== stamp_basis_70ms".to_string());
    t.push(line("drift", PLACED, 0, 1));
    t.push("== timecode_leap_80ms".to_string());
    t.push(line("leap", PLACED, 0, 1));
    // the stock debug lines: the five rows that keep OBS's 70 ms path, none from a rebased packet
    t.push("debug_lines=5".to_string());
    t
}

#[test]
fn a_locked_arrival_source_appends_and_every_other_keeps_obs_path_1425() {
    let trace = c_trace();
    assert!(
        !trace.iter().any(|l| l.starts_with("A-STEADY-BROKEN")),
        "issue 1425: a steady packet no longer appends quietly:\n{}",
        trace.join("\n")
    );
    let want = truth_table();
    for (i, (got, exp)) in trace.iter().zip(&want).enumerate() {
        assert_eq!(
            got,
            exp,
            "issue 1425: the lifted ingest diverges from the decided behaviour at trace line {i}:\n{}",
            trace.join("\n")
        );
    }
    assert_eq!(
        trace.len(),
        want.len(),
        "issue 1425: the lifted ingest printed {} lines, the truth table has {}:\n{}",
        trace.len(),
        want.len(),
        trace.join("\n")
    );
}
