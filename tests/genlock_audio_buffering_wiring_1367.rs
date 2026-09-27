//! Issue 1367 (ROZHODNUTÉ 5857354949) — the genlock audio-buffering FLOOR is wired into libobs,
//! and the shipped C behaves as decided.
//!
//! Std-only (no `camera_box::`), so it runs standalone with plain rustc:
//! `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test tests/genlock_audio_buffering_wiring_1367.rs`.
//!
//! - The wiring anchors: `obs_reset_audio2` takes the pure plan (never the caller's fixed flag),
//!   the reset log names the floor, `audio_callback` raises the floor first and keeps OBS's own
//!   dynamic increase above it, the increase is one loud named line, and the #1355 UNREACHABLE
//!   line names the buffering. The same list is required by the pwsh guard in both
//!   `windows-genlock*.yml` workflows.
//! - A C LIFT of the shipped bytes: the reset block (`obs.c`), the tick decision (`obs-audio.c`
//!   `audio_callback`) and the buffering functions (`obs-audio.c`, verbatim) compiled with a stub
//!   and driven through a launch, a late source, the low-latency toggle, 44.1 kHz and the maximum.
//!   The printed trace is the truth table. `cc` is required — it FAILS LOUDLY rather than skips.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const OBS_C: &str = "vendor/obs-studio/libobs/obs.c";
const OBS_AUDIO: &str = "vendor/obs-studio/libobs/obs-audio.c";
const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const HEADER: &str = "vendor/obs-studio/libobs/obs-genlock-audio-buffering.h";

/// `(file, squished needle)` — the wiring both this test and the pwsh guard in both
/// `windows-genlock*.yml` workflows require. ONE list, so the two copies cannot drift apart. The
/// pwsh variable per file is [`pwsh_var`].
const WIRING: [(&str, &str); 12] = [
    (OBS_C, "#include \"obs-genlock-audio-buffering.h\""),
    (OBS_C, "audio->max_buffering_ticks = (int)genlock_buf.max_ticks;"),
    (OBS_C, "audio->floor_buffering_ticks = (int)genlock_buf.floor_ticks;"),
    (OBS_C, "audio->fixed_buffer = genlock_buf.fixed;"),
    (OBS_C, "buffering type: fixed floor %d ms, dynamically increasing above"),
    (OBS_AUDIO, "#include \"obs-genlock-audio-buffering.h\""),
    (OBS_AUDIO, "const int genlock_buffering = genlock_audio_buffering_action("),
    (
        OBS_AUDIO,
        "} else if (genlock_buffering == GENLOCK_AUDIO_BUFFERING_ACTION_FLOOR) { set_floor_audio_buffering(audio, sample_rate, &ts);",
    ),
    (
        OBS_AUDIO,
        "} else if (genlock_buffering == GENLOCK_AUDIO_BUFFERING_ACTION_DYNAMIC) { add_audio_buffering(audio, sample_rate, &ts, min_ts, buffering_name);",
    ),
    (
        OBS_AUDIO,
        "const size_t total_ms = raise_audio_buffering(audio, sample_rate, ts, audio->floor_buffering_ticks);",
    ),
    (OBS_AUDIO, "genlock audio buffering ABOVE the floor (issue 1367)"),
    (HEADER, "#define GENLOCK_AUDIO_BUFFERING_FLOOR_MS 85u"),
];

/// The upstream lines the floor replaced — each must stay GONE (squished).
const RETIRED: [(&str, &str); 2] = [
    (OBS_C, "audio->fixed_buffer = oai->fixed_buffering;"),
    (
        OBS_C,
        "oai->fixed_buffering ? \"fixed\" : \"dynamically increasing\"",
    ),
];

fn pwsh_var(file: &str) -> &'static str {
    match file {
        OBS_C => "$obsc1367",
        OBS_AUDIO => "$audio1367",
        HEADER => "$hdr1367",
        other => panic!("no pwsh variable for {other}"),
    }
}

const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

#[test]
fn obs_reset_and_the_mixer_tick_carry_the_floor_1367() {
    for (file, needle) in WIRING {
        let s = squish(&read(file));
        assert_eq!(
            s.matches(needle).count(),
            1,
            "issue 1367: {file} must carry `{needle}` exactly once -- the genlock audio-buffering \
             floor is not wired (every launch would draw its own buffering again)"
        );
    }
    for (file, needle) in RETIRED {
        assert!(
            !squish(&read(file)).contains(needle),
            "issue 1367: {file} still carries the upstream `{needle}` -- the caller's fixed / \
             low-latency buffering would bypass the floor"
        );
    }
    let internal = squish(&read(OBS_INTERNAL));
    assert!(
        internal.contains("int max_buffering_ticks;")
            && internal.contains("int floor_buffering_ticks;")
            && internal.contains("bool fixed_buffer;"),
        "issue 1367: struct obs_core_audio lost floor_buffering_ticks"
    );
}

#[test]
fn every_buffering_line_keeps_the_786_launch_gate_text_1367() {
    // scripts/obs-guarded-launch.ps1, launch-obs-genlock.sh and rig-health-audit.py parse
    // `total audio buffering is now (\d+) milliseconds`; the floor line and the above-floor line
    // must both carry it, or the #786 launch gate goes blind.
    let s = read(OBS_AUDIO);
    let floor = s
        .find("\"genlock audio buffering floor (issue 1367): total audio buffering is now %d milliseconds")
        .expect("issue 1367: the floor line lost the `total audio buffering is now %d milliseconds` text");
    // Squished, with the C string-literal joins removed, so a clang-format reflow cannot break it.
    let joined = squish(&s).replace("\" \"", "");
    assert!(
        joined.contains("\"genlock audio buffering ABOVE the floor (issue 1367): adding %d milliseconds of audio buffering, total audio buffering is now %d milliseconds (source: %s)"),
        "issue 1367: the above-floor line lost the #786 `total audio buffering is now %d milliseconds` text"
    );
    let above = s
        .find("\"genlock audio buffering ABOVE the floor (issue 1367)")
        .expect("issue 1367: the above-floor line lost the #786 `total ... audio buffering is now %d milliseconds` text");
    assert!(floor < above);
    assert!(
        squish(&s).contains("blog(LOG_WARNING, \"genlock audio buffering ABOVE the floor"),
        "issue 1367: an increase above the floor must be LOUD (LOG_WARNING)"
    );
}

#[test]
fn the_unreachable_fallback_names_the_buffering_1367() {
    let s = squish(&read(OBS_SOURCE));
    assert!(
        s.contains("UNREACHABLE after %d windows outside +/-%.0fms")
            && s.contains("(fallbacks=%u) (#1355) total_audio_buffering=%dms \" \"floor=%dms (issue 1367: a safety net")
            && s.contains("#include \"obs-genlock-audio-buffering.h\""),
        "issue 1367: the #1355 UNREACHABLE line must name the global buffering and the floor \
         (the fallback is only a logged safety net now)"
    );
}

#[test]
fn the_unreachable_line_prints_total_then_floor_1367() {
    // The format names total_audio_buffering= before floor=; the arguments must follow in that
    // order (a swap would print the floor as the total and hide a late source).
    let s = squish(&read(OBS_SOURCE));
    let call = s
        .find("UNREACHABLE after %d windows outside +/-%.0fms")
        .expect("issue 1367: the UNREACHABLE line is gone");
    let end = call
        + s[call..]
            .find(");")
            .expect("issue 1367: the UNREACHABLE call does not end");
    let body = &s[call..end];
    let fmt_total = body
        .find("total_audio_buffering=%dms")
        .expect("total_audio_buffering=");
    let fmt_floor = body.find("floor=%dms").expect("floor=");
    let arg_count = body
        .find("source->asrc.level_fallback_count,")
        .expect("fallback count arg");
    let arg_total = body
        .find("(uint32_t)obs->audio.total_buffering_ticks")
        .expect("total arg");
    let arg_floor = body
        .find("(uint32_t)obs->audio.floor_buffering_ticks")
        .expect("floor arg");
    assert!(
        fmt_total < fmt_floor && arg_count < arg_total && arg_total < arg_floor,
        "issue 1367: the UNREACHABLE line's total / floor arguments are out of order"
    );
}

#[test]
fn windows_workflows_guard_the_same_wiring_1367() {
    for wf in WINDOWS_WORKFLOWS {
        let text = read(wf);
        for (file, needle) in WIRING {
            let line = format!(
                "{} -notmatch [regex]::Escape('{}')",
                pwsh_var(file),
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&line),
                "issue 1367: {wf} no longer guards `{needle}` ({file}) -- its pwsh copy drifted \
                 from WIRING"
            );
        }
        for (file, needle) in RETIRED {
            let line = format!(
                "{} -match [regex]::Escape('{}')",
                pwsh_var(file),
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&line),
                "issue 1367: {wf} no longer refuses the retired `{needle}` ({file})"
            );
        }
        for (file, var) in [
            (OBS_C, "$obsc1367"),
            (OBS_AUDIO, "$audio1367"),
            (HEADER, "$hdr1367"),
        ] {
            assert!(
                text.contains(&format!(
                    "{var} = (Get-Content \"{file}\" -Raw) -replace '\\s+', ' '"
                )),
                "issue 1367: {wf} does not load {file} into {var}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The C lift: the shipped bytes, driven.

/// The text of `src` from the first line starting with `start` up to (not including) `end`.
fn slice_between(src: &str, start: &str, end: &str) -> String {
    let a = src
        .find(start)
        .unwrap_or_else(|| panic!("issue 1367: lift anchor `{start}` not found"));
    let b = src[a..]
        .find(end)
        .unwrap_or_else(|| panic!("issue 1367: lift end `{end}` not found after `{start}`"));
    src[a..a + b].to_string()
}

/// The buffering functions of obs-audio.c, verbatim: `audio_buffering_maxed` through
/// `add_audio_buffering`.
fn lift_buffering_functions() -> String {
    slice_between(
        &read(OBS_AUDIO),
        "static inline bool audio_buffering_maxed(",
        "static bool audio_buffer_insufficient(",
    )
}

/// The mixer-tick buffering decision of `audio_callback`, verbatim.
fn lift_tick_decision() -> String {
    let s = read(OBS_AUDIO);
    let start = "\tconst int genlock_buffering = genlock_audio_buffering_action(";
    let tail = "add_audio_buffering(audio, sample_rate, &ts, min_ts, buffering_name);\n\t}\n";
    let a = s
        .find(start)
        .expect("issue 1367: the tick decision is gone");
    let b = s[a..]
        .find(tail)
        .expect("issue 1367: the tick decision's end is gone")
        + tail.len();
    s[a..a + b].to_string()
}

/// The reset block of `obs_reset_audio2`, verbatim.
fn lift_reset_block() -> String {
    let s = read(OBS_C);
    let start = "\tconst struct genlock_audio_buffering_plan genlock_buf = ";
    let tail = "\taudio->fixed_buffer = genlock_buf.fixed;\n";
    let a = s.find(start).expect("issue 1367: the reset plan is gone");
    let b = s[a..]
        .find(tail)
        .expect("issue 1367: the reset plan's end is gone")
        + tail.len();
    s[a..a + b].to_string()
}

fn c_harness() -> String {
    format!(
        r#"#include <inttypes.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "media-io/asrc-compensator.h"
#include "obs-genlock-audio-buffering.h"

#define AUDIO_OUTPUT_FRAMES 1024
#define LOG_WARNING 200
#define LOG_INFO 300
#define LOG_DEBUG 400
#define DEBUG_AUDIO 0

struct ts_info {{
	uint64_t start;
	uint64_t end;
}};
struct deque {{
	size_t pushes;
}};
static void deque_push_front(struct deque *dq, const void *data, size_t size)
{{
	(void)data;
	(void)size;
	dq->pushes++;
}}
struct obs_core_audio {{
	uint64_t buffered_ts;
	struct deque buffered_timestamps;
	uint64_t buffering_wait_ticks;
	int total_buffering_ticks;
	int max_buffering_ticks;
	int floor_buffering_ticks;
	bool fixed_buffer;
}};
struct obs_audio_info2 {{
	uint32_t samples_per_sec;
	int speakers;
	uint32_t max_buffering_ms;
	bool fixed_buffering;
}};
static inline uint64_t audio_frames_to_ns(size_t sample_rate, uint64_t frames)
{{
	return frames * 1000000000ULL / sample_rate;
}}
static inline uint64_t ns_to_audio_frames(size_t sample_rate, uint64_t ns)
{{
	return ns * sample_rate / 1000000000ULL;
}}
static void blog(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void blog(int level, const char *fmt, ...)
{{
	va_list a;
	va_start(a, fmt);
	printf("L%d ", level);
	vprintf(fmt, a);
	printf("\n");
	va_end(a);
}}

/* ---- lifted verbatim from obs-audio.c ---- */
{functions}
/* ---- end lift ---- */

static struct obs_core_audio core;

static void reset(uint32_t rate, uint32_t max_ms, bool fixed)
{{
	struct obs_core_audio *audio = &core;
	const struct obs_audio_info2 oai_v = {{rate, 2, max_ms, fixed}};
	const struct obs_audio_info2 *oai = &oai_v;
	memset(audio, 0, sizeof(*audio));
/* ---- lifted verbatim from obs.c obs_reset_audio2 ---- */
{reset}
/* ---- end lift ---- */
	printf("reset rate=%u max_ms=%u fixed=%d -> floor=%d max=%d fixed_buffer=%d\n", rate, max_ms, fixed ? 1 : 0,
	       audio->floor_buffering_ticks, audio->max_buffering_ticks, audio->fixed_buffer ? 1 : 0);
}}

/* One audio_callback tick at the real-time window [start, start + a tick]. The window it processes
 * is the front of buffered_timestamps. audio_callback pushes the real-time window and pops one
 * every tick, so the queue holds total_buffering_ticks windows ahead of it forever: the front is
 * start - total * tick (while ticks wait that is also buffered_ts - wait * tick). A source's
 * oldest audio sits behind_ms before the REAL-TIME start (0 = none behind). Then the stock
 * tail: a waiting tick is consumed. */
static void tick(size_t sample_rate, uint64_t start, uint64_t behind_ms, const char *buffering_name)
{{
	struct obs_core_audio *audio = &core;
	const uint64_t front =
		start - audio_frames_to_ns(sample_rate, (uint64_t)audio->total_buffering_ticks * AUDIO_OUTPUT_FRAMES);
	struct ts_info ts = {{front, front + audio_frames_to_ns(sample_rate, AUDIO_OUTPUT_FRAMES)}};
	const uint64_t min_ts = behind_ms ? start - behind_ms * 1000000ULL : front;
/* ---- lifted verbatim from obs-audio.c audio_callback ---- */
{decision}
/* ---- end lift ---- */
	printf("tick total=%d wait=%" PRIu64 " pushes=%zu window_back_ms=%" PRIu64 "\n", audio->total_buffering_ticks,
	       audio->buffering_wait_ticks, audio->buffered_timestamps.pushes, (uint64_t)((start - ts.start) / 1000000u));
	if (audio->buffering_wait_ticks)
		audio->buffering_wait_ticks--;
}}

static void drain(size_t sample_rate, uint64_t *t)
{{
	while (core.buffering_wait_ticks) {{
		*t += audio_frames_to_ns(sample_rate, AUDIO_OUTPUT_FRAMES);
		core.buffering_wait_ticks--;
	}}
}}

int main(void)
{{
	uint64_t t = 1000000000000ULL;
	const uint64_t tick_ns = audio_frames_to_ns(48000, AUDIO_OUTPUT_FRAMES);

	/* A launch: the first tick raises the floor; nothing behind keeps it there. */
	reset(48000, 0, false);
	tick(48000, t, 0, "-");
	t += tick_ns;
	tick(48000, t, 0, "-");
	drain(48000, &t);
	t += tick_ns;
	tick(48000, t, 0, "-");
	/* A source 136 ms behind real time: OBS's own dynamic increase above the floor (+3 ticks = 7 =
	 * ceil(136 / tick), what stock OBS reaches alone), band broken. */
	t += tick_ns;
	tick(48000, t, 136, "mbc");

	/* A source 10 ms behind real time is absorbed by the floor (the property the floor exists for);
	 * 96 ms behind adds one tick (5 = ceil(96 / tick)), band still held. */
	reset(48000, 0, false);
	t += tick_ns;
	tick(48000, t, 0, "-");
	drain(48000, &t);
	t += tick_ns;
	tick(48000, t, 10, "mbc");
	t += tick_ns;
	tick(48000, t, 96, "ASIO Input Capture");

	/* A source already behind at the FIRST tick (the stream ASIO startup race): the floor is raised
	 * first; OBS's dynamic check runs on the next, UNDRAINED ticks against the window the floor
	 * moved back. 85 ms behind is absorbed by the floor; 100 ms behind adds one tick, so the total
	 * is ceil(100 ms / tick) = 5 = what stock OBS reaches alone: max(floor, stock). */
	reset(48000, 0, false);
	t += tick_ns;
	tick(48000, t, 85, "ASIO Input Capture");
	{{
		/* Raising to the current total must be a no-op (a caller that breaks the precondition
		 * must not rewrite the window or push timestamps). */
		struct ts_info probe = {{t + 5u, t + 7u}};
		const size_t ms = raise_audio_buffering(&core, 48000, &probe, core.total_buffering_ticks);
		printf("raise-noop ms=%zu total=%d wait=%" PRIu64 " pushes=%zu ts_moved=%d\n", ms,
		       core.total_buffering_ticks, core.buffering_wait_ticks, core.buffered_timestamps.pushes,
		       (probe.start != t + 5u || probe.end != t + 7u) ? 1 : 0);
	}}
	t += tick_ns;
	tick(48000, t, 85, "ASIO Input Capture");
	t += tick_ns;
	tick(48000, t, 100, "ASIO Input Capture");

	/* The frontend low-latency toggle (fixed 20 ms): overridden, still floor then dynamic. */
	reset(48000, 20, true);
	t += tick_ns;
	tick(48000, t, 0, "-");
	drain(48000, &t);
	t += tick_ns;
	tick(48000, t, 116, "NDI test");

	/* 44.1 kHz. */
	reset(44100, 0, false);
	tick(44100, t, 0, "-");

	/* A maximum at the floor: nothing grows above it, even for a source 136 ms behind. */
	reset(48000, 85, false);
	t += tick_ns;
	tick(48000, t, 0, "-");
	drain(48000, &t);
	t += tick_ns;
	tick(48000, t, 136, "mbc");

	/* A late source past the maximum: clamped, both loud lines. */
	reset(48000, 150, false);
	t += tick_ns;
	tick(48000, t, 0, "-");
	drain(48000, &t);
	t += tick_ns;
	tick(48000, t, 286, "sp-slow_video");
	return 0;
}}
"#,
        functions = lift_buffering_functions(),
        reset = lift_reset_block(),
        decision = lift_tick_decision()
    )
}

fn scratch_dir() -> PathBuf {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!(
        "genlock_audio_buffering_1367_{}",
        std::process::id()
    ))
}

/// The lift's printed trace, compiled and run ONCE per test binary: the tests run on parallel
/// threads, and two of them compiling into (and cleaning) one scratch dir raced each other.
fn c_trace() -> &'static [String] {
    static TRACE: OnceLock<Vec<String>> = OnceLock::new();
    TRACE.get_or_init(build_and_run_lift)
}

fn build_and_run_lift() -> Vec<String> {
    let dir = scratch_dir();
    fs::create_dir_all(&dir).expect("create the lift scratch dir");
    let harness = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&harness, c_harness()).expect("write the lift harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg("-I")
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&harness)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1367: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 shipped audio-buffering C; it must FAIL rather than skip. Install a C compiler or \
                 set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1367: the lifted obs.c / obs-audio.c buffering code does NOT COMPILE standalone \
         under -Wall -Wextra -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1367: the compiled lift harness failed to execute");
    assert!(
        run.status.success(),
        "issue 1367: the lift harness exited non-zero"
    );
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

const BROKEN_TAIL: &str =
    " -- a mixed source on an absolute ASRC level target (#1335/#1355, e.g. the stream mbc) cannot reach it at this buffering";

fn expected_trace() -> Vec<String> {
    let floor_line = |ms: u32| {
        format!(
            "L300 genlock audio buffering floor (issue 1367): total audio buffering is now {ms} \
             milliseconds from the first audio tick, dynamically increasing above"
        )
    };
    let above = |add: u32, total: u32, src: &str, band: &str, err: &str, tail: &str| {
        format!(
            "L200 genlock audio buffering ABOVE the floor (issue 1367): adding {add} milliseconds of \
             audio buffering, total audio buffering is now {total} milliseconds (source: {src}); \
             ASRC level band {band}: buffering + 9 ms base lands {err} ms off the 100 ms level \
             target (reach +/-35 ms){tail}"
        )
    };
    vec![
        // a launch
        "reset rate=48000 max_ms=0 fixed=0 -> floor=4 max=45 fixed_buffer=0".into(),
        floor_line(85),
        "tick total=4 wait=4 pushes=4 window_back_ms=85".into(),
        "tick total=4 wait=3 pushes=4 window_back_ms=85".into(),
        "tick total=4 wait=0 pushes=4 window_back_ms=85".into(),
        above(64, 149, "mbc", "BROKEN", "-58.3", BROKEN_TAIL),
        "tick total=7 wait=3 pushes=7 window_back_ms=149".into(),
        // 10 ms behind real time absorbed, 96 ms adds one tick
        "reset rate=48000 max_ms=0 fixed=0 -> floor=4 max=45 fixed_buffer=0".into(),
        floor_line(85),
        "tick total=4 wait=4 pushes=4 window_back_ms=85".into(),
        "tick total=4 wait=0 pushes=4 window_back_ms=85".into(),
        above(21, 106, "ASIO Input Capture", "ok", "-15.7", ""),
        "tick total=5 wait=1 pushes=5 window_back_ms=106".into(),
        // late at the first tick, then undrained: 85 ms absorbed, 100 ms adds one tick
        "reset rate=48000 max_ms=0 fixed=0 -> floor=4 max=45 fixed_buffer=0".into(),
        floor_line(85),
        "tick total=4 wait=4 pushes=4 window_back_ms=85".into(),
        "raise-noop ms=85 total=4 wait=3 pushes=4 ts_moved=0".into(),
        "tick total=4 wait=3 pushes=4 window_back_ms=85".into(),
        above(21, 106, "ASIO Input Capture", "ok", "-15.7", ""),
        "tick total=5 wait=3 pushes=5 window_back_ms=106".into(),
        // the low-latency toggle
        "reset rate=48000 max_ms=20 fixed=1 -> floor=4 max=45 fixed_buffer=0".into(),
        floor_line(85),
        "tick total=4 wait=4 pushes=4 window_back_ms=85".into(),
        above(42, 128, "NDI test", "BROKEN", "-37.0", BROKEN_TAIL),
        "tick total=6 wait=2 pushes=6 window_back_ms=127".into(),
        // 44.1 kHz
        "reset rate=44100 max_ms=0 fixed=0 -> floor=4 max=45 fixed_buffer=0".into(),
        floor_line(92),
        "tick total=4 wait=4 pushes=4 window_back_ms=92".into(),
        // a maximum at the floor
        "reset rate=48000 max_ms=85 fixed=0 -> floor=4 max=4 fixed_buffer=0".into(),
        floor_line(85),
        "tick total=4 wait=4 pushes=4 window_back_ms=85".into(),
        "tick total=4 wait=0 pushes=4 window_back_ms=85".into(),
        // past the maximum
        "reset rate=48000 max_ms=150 fixed=0 -> floor=4 max=8 fixed_buffer=0".into(),
        floor_line(85),
        "tick total=4 wait=4 pushes=4 window_back_ms=85".into(),
        "L200 Max audio buffering reached!".into(),
        above(85, 170, "sp-slow_video", "BROKEN", "-79.7", BROKEN_TAIL),
        "tick total=8 wait=4 pushes=8 window_back_ms=170".into(),
    ]
}

#[test]
fn the_shipped_c_starts_every_launch_at_the_floor_and_grows_above_it_1367() {
    let c = c_trace();
    let e = expected_trace();
    for (i, (cl, el)) in c.iter().zip(&e).enumerate() {
        assert_eq!(
            cl, el,
            "issue 1367: the lifted C diverges from the decided behaviour at line {i}"
        );
    }
    assert_eq!(
        c.len(),
        e.len(),
        "issue 1367: the lifted C printed {} lines, the truth table has {}:\n{}",
        c.len(),
        e.len(),
        c.join("\n")
    );
}

#[test]
fn the_786_launch_gate_reads_the_floor_as_clean_1367() {
    // The #786 gates' own regex over the trace: the floor alone (85 / 92 ms) stays under their
    // 100 ms bound, and a real late-source increase above it still trips them.
    let c = c_trace();
    let totals: Vec<u32> = c
        .iter()
        .filter_map(|l| {
            let rest = l.split("total audio buffering is now ").nth(1)?;
            rest.split(' ').next()?.parse().ok()
        })
        .collect();
    assert!(totals.contains(&85) && totals.contains(&92));
    assert!(totals
        .iter()
        .filter(|&&t| t == 85 || t == 92)
        .all(|&t| t <= 100));
    assert!(
        totals.iter().any(|&t| t > 100),
        "the trace must also show an increase the gate flags"
    );
}
