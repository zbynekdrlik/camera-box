//! Issue 1381 -- the audio-thread stall probe names WHAT took the time (design 6191167734).
//!
//! Live on stream-snv (15.10.2026) the `audio-stall #1367:` line read `tick_gap_max_ms` 100-209
//! while the mix itself (`callback_max_ms`) never took more than ~1.8 ms. The rest of one
//! audio-thread iteration is in `media-io/audio-io.c`: the sleep to the tick, the input mutex and
//! every output's callback. These tests pin that the iteration is now measured there (window
//! maxima on `struct audio_output`), that the 60 s dump in obs-audio.c takes them, names the
//! slowest callback without ever waiting for a lock, and appends them to the same line.
//!
//! Std-only: runs under `cargo test` and standalone with
//! `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test tests/audio_stall_out_1381.rs`.
//! The behavioural part compiles the REAL audio-io.c into `tests/c/audio_stall_out_1381_harness.c`
//! (fake clock, scripted lock waits and callback durations) together with two pieces of
//! obs-audio.c lifted verbatim, and fails loudly when no C compiler is present.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

const AUDIO_IO_C: &str = "vendor/obs-studio/libobs/media-io/audio-io.c";
const AUDIO_IO_H: &str = "vendor/obs-studio/libobs/media-io/audio-io.h";
const OBS_AUDIO: &str = "vendor/obs-studio/libobs/obs-audio.c";
const LIBOBS: &str = "vendor/obs-studio/libobs";
const HARNESS: &str = "tests/c/audio_stall_out_1381_harness.c";

const IDENTITY_SIG: &str = "static const char *audio_stall_out_cb_identity(";
const STALL_FMT: &str = "\"audio-stall #1367: tick_gap_max_ms=";

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("issue 1381: read {rel}: {e}"))
}

/// A `static` function, by its signature, through its first top-level closing brace.
fn lift_fn(src: &str, signature: &str) -> String {
    assert_eq!(
        src.matches(signature).count(),
        1,
        "issue 1381: `{signature}` must be defined exactly once"
    );
    let a = src.find(signature).unwrap();
    let b = src[a..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("issue 1381: `{signature}` does not end"));
    src[a..a + b + 3].to_string()
}

/// The dump's `blog(...)` statement that writes the `audio-stall #1367:` line, verbatim.
fn lift_stall_log(src: &str) -> String {
    assert_eq!(
        src.matches(STALL_FMT).count(),
        1,
        "issue 1381: the audio-stall format must exist exactly once in obs-audio.c"
    );
    let fmt = src.find(STALL_FMT).unwrap();
    let a = src[..fmt]
        .rfind("blog(")
        .expect("issue 1381: the audio-stall line is not a blog() call");
    let b = src[fmt..]
        .find(");\n")
        .expect("issue 1381: the audio-stall blog() call does not end");
    src[a..fmt + b + 3].to_string()
}

/// One compile + run of the harness per test binary (two tests compiling into one scratch dir
/// raced in the issue-1367 lift); a per-process counter keeps parallel test processes apart.
fn harness_trace() -> &'static Vec<String> {
    static TRACE: OnceLock<Vec<String>> = OnceLock::new();
    TRACE.get_or_init(|| {
        static N: AtomicUsize = AtomicUsize::new(0);
        let obs_audio = read(OBS_AUDIO);
        let template = read(HARNESS);
        for marker in ["@IDENTITY_FN@", "@STALL_LOG@"] {
            assert_eq!(
                template.matches(marker).count(),
                1,
                "issue 1381: {HARNESS} must carry {marker} exactly once"
            );
        }
        let src = template
            .replace("@IDENTITY_FN@", &lift_fn(&obs_audio, IDENTITY_SIG))
            .replace("@STALL_LOG@", &lift_stall_log(&obs_audio));
        let tag = format!(
            "audio-stall-out-1381-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        );
        let dir = std::env::temp_dir().join(&tag);
        fs::create_dir_all(&dir).expect("issue 1381: scratch dir");
        let c = dir.join("harness.c");
        let bin = dir.join("harness");
        fs::write(&c, src).expect("issue 1381: write harness");
        let audio_io = repo().join(AUDIO_IO_C);
        let out = Command::new("cc")
            .args(["-std=gnu11", "-Wall", "-Wextra", "-Wformat=2", "-Werror"])
            .arg("-Wno-unused-function")
            .arg(format!("-I{}", repo().join(LIBOBS).display()))
            .arg(format!("-DAUDIO_IO_C=\"{}\"", audio_io.display()))
            .arg(&c)
            .arg("-o")
            .arg(&bin)
            .args(["-lpthread", "-lm"])
            .output()
            .expect("issue 1381: a C compiler (cc) is required -- this gate never skips");
        assert!(
            out.status.success(),
            "issue 1381: the harness does not compile:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let run = Command::new(&bin)
            .output()
            .expect("issue 1381: run the harness");
        let _ = fs::remove_dir_all(&dir);
        assert!(
            run.status.success(),
            "issue 1381: the harness failed: {}",
            String::from_utf8_lossy(&run.stdout)
        );
        String::from_utf8_lossy(&run.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    })
}

fn trace_line(prefix: &str) -> String {
    let want = format!("{prefix} ");
    let hits: Vec<&String> = harness_trace()
        .iter()
        .filter(|l| l.starts_with(&want))
        .collect();
    assert_eq!(hits.len(), 1, "issue 1381: one `{prefix}` line expected");
    hits[0].clone()
}

/// The sleep, the input mutex, the output pass and the slowest output callback of one window,
/// measured in the real audio-io.c. Window 1, from S = the clock at audio_thread entry (tick k is
/// due at S + floor(k * 1024e9 / 48000) = S + 21333333, 42666666, 64000000, 85333333):
/// - tick 1 sleeps and wakes 30 ms late (S + 51333333): wake 30 ms, sleep-over 30 ms; then
///   mix 1 ms, A 2 ms, B 5 ms: ends S + 59333333.
/// - tick 2 is already behind (no sleep): wake 16666667; mix 1, A 80, B 1: pass 81 ms, ends
///   S + 141333333.
/// - tick 3 behind: wake 141333333 - 64000000 = 77333333 (the maximum); mix 1, then a 12 ms wait
///   for mix 0's input mutex, A 1, B 1: pass 14 ms, ends S + 156333333.
/// - tick 4 behind: wake 71000000; mix 1, A 3, B 1 -- B is the LAST callback, A stays the slowest.
#[test]
fn the_window_names_the_sleep_the_lock_the_pass_and_the_slowest_output_1381() {
    assert_eq!(
        trace_line("W1"),
        "W1 wake=77333333 sleep=30000000 lock=12000000 pass=81000000 cb=80000000 fn=A param=PA mix=0"
    );
}

/// The take resets the window; a second take reads zeros and no callback identity.
#[test]
fn a_take_resets_the_window_1381() {
    assert_eq!(
        trace_line("W1R"),
        "W1R wake=0 sleep=0 lock=0 pass=0 cb=0 fn=- param=- mix=0"
    );
}

/// Window 2: one tick that wakes ON time (no lateness at all), waits 5 ms for the get-mixers lock
/// (outside the output pass), and B on mix 1 is the slowest output: the new window carries
/// nothing of window 1.
#[test]
fn a_new_window_starts_clean_and_measures_the_get_mixers_lock_1381() {
    assert_eq!(
        trace_line("W2"),
        "W2 wake=0 sleep=0 lock=5000000 pass=6000000 cb=4000000 fn=B param=PB mix=1"
    );
}

/// The dump names the slowest callback's param by walking the output and encoder lists under a
/// TRYLOCK only (the dump runs on the audio thread): a held list never makes the thread wait.
#[test]
fn the_dump_names_the_slowest_callback_without_ever_waiting_for_a_lock_1381() {
    let expect = [
        "ID none kind=- name='' held=0 blocked=0",
        "ID out1 kind=output name='NDI PGM' held=0 blocked=0",
        "ID out2 kind=output name='Rec' held=0 blocked=0",
        "ID enc kind=encoder name='aac'enc' held=0 blocked=0",
        "ID raw kind=raw name='' held=0 blocked=0",
        "ID outbusy-enc kind=encoder name='aac'enc' held=0 blocked=0",
        "ID outbusy-raw kind=busy name='' held=0 blocked=0",
        "ID encbusy-raw kind=busy name='' held=0 blocked=0",
        "ID encbusy-out kind=output name='NDI PGM' held=0 blocked=0",
        "ID trunc kind=output name='NDI' held=0 blocked=0",
        "ID noname kind=output name='' held=0 blocked=0",
    ];
    let got: Vec<&String> = harness_trace()
        .iter()
        .filter(|l| l.starts_with("ID "))
        .collect();
    assert_eq!(got, expect.iter().collect::<Vec<_>>());
    let ident = lift_fn(&read(OBS_AUDIO), IDENTITY_SIG);
    assert!(
        !ident.contains("pthread_mutex_lock("),
        "issue 1381: the name resolver runs on the audio thread and must never block on a lock"
    );
    for list in ["&obs->data.outputs_mutex", "&obs->data.encoders_mutex"] {
        assert!(
            ident.contains(&format!("pthread_mutex_trylock({list})")),
            "issue 1381: the name resolver must only TRY {list}"
        );
    }
}

/// The exact line the supervisor reads on the box: the 1367 fields unchanged, then the new ones.
#[test]
fn the_audio_stall_line_appends_the_attribution_fields_1381() {
    assert_eq!(
        trace_line("LINE"),
        "LINE audio-stall #1367: tick_gap_max_ms=101.4 callback_max_ms=1.7 ticks=2816 ticks_over=441 \
         tick_ms=21.3 wake_late_max_ms=95.3 sleep_over_max_ms=3.1 lock_wait_max_ms=12.3 \
         out_pass_max_ms=88.9 out_cb_max_ms=80.1 out_cb_mix=1 out_cb_kind=output \
         out_cb_fn=0x7ff612345678 out_cb_param=0xdeadbeef00 out_cb_name='NDI PGM'"
    );
}

/// The take and the name walk run ONLY in the 60 s dump, never per tick: each is called once,
/// inside the rate-limited block, before the line is written.
#[test]
fn the_attribution_is_read_only_on_the_dump_path_1381() {
    let src = read(OBS_AUDIO);
    assert_eq!(src.matches("audio_output_take_stall_stats(").count(), 1);
    assert_eq!(src.matches("audio_stall_out_cb_identity(").count(), 2);
    let gate = src
        .find("if (t800_now - t800_last_log_ns >= 60000000000ULL) {")
        .expect("issue 1381: the #800 60 s gate is gone");
    let take = src
        .find("audio_output_take_stall_stats(audio->audio, &")
        .unwrap();
    // the call site (the definition reads `*audio_stall_out_cb_identity(const void`); short and
    // wrap-independent
    assert_eq!(src.matches("= audio_stall_out_cb_identity(").count(), 1);
    let call = src.find("= audio_stall_out_cb_identity(").unwrap();
    let line = src.find(STALL_FMT).unwrap();
    assert!(
        gate < take && take < call && call < line,
        "issue 1381: take -> name -> line must run inside the 60 s dump, in that order"
    );
    let io = read(AUDIO_IO_C);
    assert_eq!(
        io.matches("audio_output_take_stall_stats(").count(),
        1,
        "issue 1381: audio-io.c defines the take and never calls it on its own"
    );
}

/// The per-tick measurement in audio-io.c: no allocation, no log, no lock of its own (the one
/// lock in it is the input mutex upstream already takes there), and the getter is declared.
#[test]
fn the_per_tick_measurement_never_allocates_logs_or_takes_a_new_lock_1381() {
    let io = read(AUDIO_IO_C);
    let begin = "/* camera-box issue 1381 BEGIN";
    let end = "/* camera-box issue 1381 END */";
    assert_eq!(io.matches(begin).count(), 1);
    assert_eq!(io.matches(end).count(), 1);
    let a = io.find(begin).unwrap();
    let block = &io[a..io.find(end).unwrap()];
    for banned in [
        "blog(", "bmalloc", "bzalloc", "brealloc", "da_push", "trylock",
    ] {
        assert!(
            !block.contains(banned),
            "issue 1381: the per-tick stall block must not use `{banned}`"
        );
    }
    assert_eq!(
        block.matches("pthread_mutex_lock(").count(),
        1,
        "issue 1381: the only lock in the block is the upstream input mutex"
    );
    assert!(block.contains("pthread_mutex_lock(&audio->input_mutex);"));
    let h = read(AUDIO_IO_H);
    assert!(h.contains("struct audio_output_stall_stats {"));
    assert!(h.contains(
        "void audio_output_take_stall_stats(audio_t *audio, struct audio_output_stall_stats *stats);"
    ));
}
