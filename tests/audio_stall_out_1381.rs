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
const COPY_NAME_SIG: &str = "static void audio_stall_copy_name(";
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

/// `text` without its `#ifdef DEBUG_AUDIO` .. `#endif` blocks (upstream's compiled-out debug lines).
fn strip_debug_audio(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(a) = rest.find("#ifdef DEBUG_AUDIO") {
        out.push_str(&rest[..a]);
        let b = rest[a..]
            .find("#endif")
            .expect("issue 1381: an unterminated DEBUG_AUDIO block");
        rest = &rest[a + b + "#endif".len()..];
    }
    out.push_str(rest);
    out
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
            .replace(
                "@IDENTITY_FN@",
                &format!(
                    "{}\n{}",
                    lift_fn(&obs_audio, COPY_NAME_SIG),
                    lift_fn(&obs_audio, IDENTITY_SIG)
                ),
            )
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

/// The sleep, the input mutex, the output pass, the tail and the slowest output callback of one
/// window, measured in the real audio-io.c, plus the breakdown behind the worst late wake. Window 1,
/// from S = the clock at audio_thread entry (tick k is due at S + floor(k * 1024e9 / 48000) =
/// S + 21333333, 42666666, 64000000, 85333333):
/// - tick 1 sleeps and wakes 30 ms late (S + 51333333): wake 30 ms, sleep-over 30 ms; then
///   mix 1 ms, A 2 ms, B 5 ms: ends S + 59333333 (busy 8 ms).
/// - tick 2 is already behind (no sleep): wake 16666667; mix 1, A 80, a 12 ms wait for mix 1's
///   input mutex, B 1: pass 93 ms, then a 4 ms profiler tail: ends S + 157333333 (busy 98 ms).
/// - tick 3 behind: wake 157333333 - 64000000 = 93333333, the worst, so the breakdown is tick 2's
///   (did not sleep; busy 98, mix 1, pass 93, lock 12, tail 4); mix 1, A 1, B 1: ends S + 160333333.
/// - tick 4 behind: wake 75000000; mix 1, A 3, B 1 -- B is the LAST callback, A stays the slowest.
#[test]
fn the_window_names_the_sleep_the_lock_the_pass_and_the_slowest_output_1381() {
    assert_eq!(
        trace_line("W1"),
        "W1 wake=93333333 sleep=30000000 lock=12000000 pass=93000000 cb=80000000 fn=A param=PA \
         mix=0 tail=4000000 lslept=0 pbusy=98000000 pmix=1000000 ppass=93000000 plock=12000000 \
         ptail=4000000"
    );
}

/// The take resets the window; a second take reads zeros and no callback identity.
#[test]
fn a_take_resets_the_window_1381() {
    assert_eq!(
        trace_line("W1R"),
        "W1R wake=0 sleep=0 lock=0 pass=0 cb=0 fn=- param=- mix=0 tail=0 lslept=0 pbusy=0 pmix=0 \
         ppass=0 plock=0 ptail=0"
    );
}

/// Window 2: one tick that SLEPT and the OS woke 2 ms late -- the worst wake of this window, so
/// `lslept=1` and the breakdown is window 1's last iteration (tick 4: busy 5 = mix 1 + A 3 + B 1,
/// pass 4). It waits 5 ms for the get-mixers lock (outside the output pass), and B on mix 1 is the
/// slowest output: the new window carries no maximum of window 1.
#[test]
fn a_new_window_starts_clean_and_measures_the_get_mixers_lock_1381() {
    assert_eq!(
        trace_line("W2"),
        "W2 wake=2000000 sleep=2000000 lock=5000000 pass=6000000 cb=4000000 fn=B param=PB mix=1 \
         tail=0 lslept=1 pbusy=5000000 pmix=1000000 ppass=4000000 plock=0 ptail=0"
    );
}

/// Window 3: B (mix 1, before C) takes 9 ms and disconnects ITSELF inside its callback, which moves
/// C into its array slot. The identity is read before the call, so the window names B, not C. A
/// tick that wakes exactly on time is not late (no breakdown).
#[test]
fn a_callback_that_disconnects_itself_is_still_named_1381() {
    assert_eq!(
        trace_line("W3"),
        "W3 wake=0 sleep=0 lock=0 pass=12000000 cb=9000000 fn=B param=PB mix=1 tail=0 lslept=0 \
         pbusy=0 pmix=0 ppass=0 plock=0 ptail=0"
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
        "ID raw kind=unlisted name='' held=0 blocked=0",
        "ID outbusy-enc kind=encoder name='aac'enc' held=0 blocked=0",
        "ID outbusy-raw kind=busy name='' held=0 blocked=0",
        "ID encbusy-raw kind=busy name='' held=0 blocked=0",
        "ID encbusy-out kind=output name='NDI PGM' held=0 blocked=0",
        "ID trunc kind=output name='NDI' held=0 blocked=0",
        "ID noname kind=output name='' held=0 blocked=0",
        "ID ctl kind=output name='a?b?c' held=0 blocked=0",
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
         out_pass_max_ms=88.9 tail_max_ms=5.6 late_slept=1 late_prev_busy_ms=98.1 \
         late_prev_mix_ms=1.0 late_prev_pass_ms=93.1 late_prev_lock_ms=12.4 late_prev_tail_ms=4.0 \
         out_cb_max_ms=80.1 out_cb_mix=1 out_cb_kind=output out_cb_fn=0x7ff612345678 \
         out_cb_param=0xdeadbeef00 out_cb_name='NDI PGM'"
    );
}

/// The take and the name walk run ONLY in the 60 s dump, never per tick: the stall line is written
/// by ONE helper, `audio_stall_dump`, called once inside the rate-limited #800 block (before its
/// per-source walk); inside it the take, the name and the line run in that order.
#[test]
fn the_attribution_is_read_only_on_the_dump_path_1381() {
    let src = read(OBS_AUDIO);
    assert_eq!(src.matches("audio_output_take_stall_stats(").count(), 1);
    assert_eq!(src.matches("audio_stall_out_cb_identity(").count(), 2);
    assert_eq!(src.matches("audio_stall_dump(").count(), 2);
    let gate = src
        .find("if (t800_now - t800_last_log_ns >= 60000000000ULL) {")
        .expect("issue 1381: the #800 60 s gate is gone");
    let dump_call = "audio_stall_dump(audio->audio, sample_rate);";
    assert_eq!(src.matches(dump_call).count(), 1);
    let call_site = src.find(dump_call).unwrap();
    let walk = src
        .find("struct obs_source *tsrc = data->first_audio_source;")
        .expect("issue 1381: the #800 per-source walk is gone");
    assert!(
        gate < call_site && call_site < walk,
        "issue 1381: the stall line must be written inside the 60 s dump, before its per-source walk"
    );
    let helper = lift_fn(&src, "static void audio_stall_dump(");
    let take = helper
        .find("audio_output_take_stall_stats(audio_io, &")
        .expect("issue 1381: the helper no longer takes the audio-io window");
    // the call site (the definition reads `*audio_stall_out_cb_identity(const void`); short and
    // wrap-independent
    let name = helper
        .find("= audio_stall_out_cb_identity(")
        .expect("issue 1381: the helper no longer names the slowest callback");
    let line = helper
        .find(STALL_FMT)
        .expect("issue 1381: the helper no longer writes the stall line");
    assert!(
        take < name && name < line,
        "issue 1381: take -> name -> line, in that order"
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
    // the three upstream functions the measurement is wired into (their DEBUG_AUDIO-only debug
    // lines aside): no log, no allocation, and the input mutex only through the measured lock
    for sig in [
        "static inline void do_audio_output(",
        "static void input_and_output(",
        "static void *audio_thread(",
    ] {
        let body = strip_debug_audio(&lift_fn(&io, sig));
        for banned in [
            "blog(",
            "bmalloc",
            "bzalloc",
            "brealloc",
            "da_push",
            "trylock",
            "pthread_mutex_lock(",
        ] {
            assert!(
                !body.contains(banned),
                "issue 1381: `{sig}` must not use `{banned}` on the audio thread's tick"
            );
        }
    }
    assert_eq!(io.matches("audio_stall_lock_input(audio);").count(), 2);
    let h = read(AUDIO_IO_H);
    assert!(h.contains("struct audio_output_stall_stats {"));
    assert!(h.contains(
        "void audio_output_take_stall_stats(audio_t *audio, struct audio_output_stall_stats *stats);"
    ));
}
