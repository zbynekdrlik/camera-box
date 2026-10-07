//! Issue 1372 part B — the WIRING of the receive-FIFO relabel in the vendored libobs.
//!
//! The decision is parity-gated against the Rust authority (`tests/genlock_fifo_relabel_parity_1372.rs`);
//! this file pins where obs-source.c calls it and runs that glue:
//! - the producer push relabels an arriving old-epoch frame BEFORE the stamp tracker and the
//!   arrival lag read it, and the explicit flush closes an open window;
//! - the release reads its one `wall_now` between two monotonic reads and books + applies the step
//!   BEFORE the deadline, the due scan and the backward-step guard read the queue;
//! - the `genlock-fifo audit` line prints `relabelled=` right after `n2_early=`;
//! - the per-source state lives in obs_source (obs-internal.h includes the header);
//! - both `windows-genlock*.yml` workflows guard the same needles (ONE list, `RELABEL_WIRING`).
//!
//! The glue (`genlock_fifo_relabel_tick` + the box booking + the queue callbacks, and the producer
//! statement) is LIFTED verbatim, compiled against the real header with a stub `obs_source_t` and a
//! printf-checked `blog`, and driven through a step: `-Wformat=2` checks every log format against
//! its real arguments. Std-only (no `camera_box` import), so it runs with plain `rustc --test`.
//! `cc` is required — it FAILS LOUDLY rather than skipping when the toolchain is missing.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The obs-source.c wiring both the Rust guard and the pwsh guard in both `windows-genlock*.yml`
/// workflows require (squished). ONE list, so the copies cannot drift.
const RELABEL_WIRING: [&str; 7] = [
    "#include \"obs-genlock-fifo-relabel.h\"",
    "output->timestamp = genlock_fifo_relabel_receive(&source->genlock_relabel, source->genlock_rx_last_ts, output->timestamp, source->genlock_rx_min_delta_ns, os_gettime_ns());",
    "const uint64_t relabel_mono_before = os_gettime_ns(); const uint64_t wall_now = genlock_wall_now_ns(); const uint64_t relabel_mono_after = os_gettime_ns(); genlock_fifo_relabel_tick(source, relabel_mono_before, wall_now, relabel_mono_after, interval, reserve_ms);",
    "static struct genlock_fifo_relabel_booking genlock_relabel_booking;",
    "source->genlock_relabel.arrival.old_epoch = false; source->genlock_relabel.jump_ns = 0;",
    "\"relabelled=%llu \"",
    "(unsigned long long)source->genlock_n2_early, (unsigned long long)source->genlock_relabel.relabelled,",
];

/// The obs-internal.h half (the include that brings the state type, and the per-source field).
const RELABEL_INTERNAL: [&str; 2] = [
    "#include \"obs-genlock-fifo-relabel.h\"",
    "struct genlock_fifo_relabel_state genlock_relabel;",
];

const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

#[test]
fn the_fifo_calls_the_relabel_where_the_design_puts_it_1372() {
    let raw = read(OBS_SOURCE);
    let src = squish(&raw);
    for needle in RELABEL_WIRING {
        assert_eq!(
            src.matches(needle).count(),
            1,
            "issue 1372: {OBS_SOURCE} must carry `{needle}` exactly once"
        );
    }
    let internal = squish(&read(OBS_INTERNAL));
    for needle in RELABEL_INTERNAL {
        assert!(
            internal.contains(needle),
            "issue 1372: {OBS_INTERNAL} lost `{needle}`"
        );
    }
    // The producer relabels BEFORE the stamp tracker and the arrival lag read the stamp.
    let at = |needle: &str| {
        src.find(needle)
            .unwrap_or_else(|| panic!("{OBS_SOURCE}: `{needle}` is gone"))
    };
    let receive = at(RELABEL_WIRING[1]);
    assert!(
        receive < at("genlock_stamp_track_observe(&source->genlock_rx_last_ts,")
            && receive < at("source->genlock_rx_arrival_lag_ns = rx_wall > output->timestamp"),
        "issue 1372: an arriving old-epoch frame must be relabelled before the stamp tracker and the \
         arrival lag read it"
    );
    // The release books + applies before the deadline, the due scan and the backward-step guard.
    let tick = at(RELABEL_WIRING[2]);
    for later in [
        "genlock_present_ts_reserve(wall_now, reserve_ms)",
        "source->async_frames.array[due]->timestamp <=",
        "uint64_t max_ts = source->async_frames.array[0]->timestamp;",
        "return genlock_release_tick(source, wall_now, present_ts, due, interval, reserve_ms, now_ns);",
    ] {
        assert!(
            tick < at(later),
            "issue 1372: the relabel must run before `{later}` reads the queue"
        );
    }
    // relabelled= sits right after n2_early= on the audit format string, once, and its key is
    // mutually non-substring with every other key on the line.
    let start = raw
        .find("\"genlock-fifo audit '%s':")
        .expect("the audit format string");
    let tail = &raw[start..];
    let end = tail.find("\"(#70/").expect("the audit ticket list");
    let fmt = &tail[..end];
    let keys: Vec<&str> = fmt
        .split(|c: char| c.is_whitespace() || c == '"')
        .filter(|t| {
            t.ends_with("=%llu")
                || t.ends_with("=%u")
                || t.ends_with("=%d")
                || t.ends_with("=%lld")
                || t.ends_with("=%s")
                || t.ends_with("=%zu")
        })
        .map(|t| t.split('=').next().expect("a key"))
        .collect();
    let i = keys
        .iter()
        .position(|k| *k == "relabelled")
        .expect("relabelled= on the audit line");
    assert_eq!(keys[i - 1], "n2_early", "relabelled= must follow n2_early=");
    assert_eq!(keys.iter().filter(|k| **k == "relabelled").count(), 1);
    for k in keys.iter().filter(|k| **k != "relabelled") {
        assert!(
            !k.contains("relabelled") && !"relabelled".contains(k),
            "`relabelled=` collides with the audit key `{k}`"
        );
    }
}

#[test]
fn windows_workflows_guard_the_same_relabel_wiring_1372() {
    for wf in WINDOWS_WORKFLOWS {
        let text = read(wf);
        for needle in RELABEL_WIRING {
            assert!(
                text.contains(&format!("$src -notmatch [regex]::Escape('{needle}')")),
                "issue 1372: {wf} no longer guards `{needle}` in obs-source.c — its pwsh copy of \
                 the_fifo_calls_the_relabel_where_the_design_puts_it_1372 drifted"
            );
        }
        for needle in RELABEL_INTERNAL {
            assert!(
                text.contains(&format!("$internal -notmatch [regex]::Escape('{needle}')")),
                "issue 1372: {wf} no longer guards `{needle}` in obs-internal.h"
            );
        }
    }
}

/// The glue lifted verbatim: from the booking's comment to the end of `genlock_fifo_relabel_tick`.
fn lift_glue(raw: &str) -> &str {
    let start = raw
        .find("/* camera-box issue 1372 part B (design 6026394143): the box-wide wall-step booking")
        .expect("the relabel glue is gone");
    let end = raw[start..]
        .find("static bool ready_async_frame(obs_source_t *source, uint64_t sys_time)\n{")
        .expect("ready_async_frame follows the glue");
    &raw[start..start + end]
}

/// The producer statement lifted verbatim.
fn lift_receive(raw: &str) -> &str {
    let start = raw
        .find("output->timestamp = genlock_fifo_relabel_receive(")
        .expect("the producer relabel is gone");
    let end = raw[start..].find(");").expect("the statement ends") + 2;
    &raw[start..start + end]
}

#[test]
fn the_lifted_glue_relabels_a_step_and_logs_it_1372() {
    let raw = read(OBS_SOURCE);
    let harness = format!(
        r#"#include <inttypes.h>
#include <stdarg.h>
#include <stdio.h>
#include "obs-genlock-fifo-relabel.h"

#define LOG_INFO 300
struct obs_source_frame {{
	uint64_t timestamp;
}};
typedef struct obs_source {{
	struct {{
		struct obs_source_frame **array;
		size_t num;
	}} async_frames;
	struct {{
		const char *name;
	}} context;
	uint64_t genlock_locked_next_boundary_ns;
	uint64_t genlock_rx_last_ts;
	uint64_t genlock_rx_min_delta_ns;
	struct genlock_fifo_relabel_state genlock_relabel;
}} obs_source_t;

static uint64_t fake_mono = 123456789000000ULL;
static uint64_t os_gettime_ns(void)
{{
	return fake_mono;
}}

static void blog(int level, const char *format, ...) __attribute__((format(printf, 2, 3)));
static void blog(int level, const char *format, ...)
{{
	va_list args;
	va_start(args, format);
	printf("LOG %d ", level);
	vprintf(format, args);
	printf("\n");
	va_end(args);
}}

{glue}
static void receive(obs_source_t *source, struct obs_source_frame *output)
{{
	{receive}
	source->genlock_rx_last_ts = output->timestamp;
}}

int main(void)
{{
	const uint64_t i30 = 33333333ULL;
	const int64_t step = 1600000000LL;
	const uint64_t w0 = 1791338400000000000ULL;
	struct obs_source_frame frames[4];
	struct obs_source_frame *array[4];
	for (size_t k = 0; k < 4; k++) {{
		frames[k].timestamp = w0 - (4 - k) * i30;
		array[k] = &frames[k];
	}}
	obs_source_t src = {{{{array, 4}}, {{"NDI 2ME PGM"}}, w0 - 4 * i30, w0 - i30, i30, {{0}}}};
	/* the tick before the step seeds the booking; the one after books + applies it */
	genlock_fifo_relabel_tick(&src, fake_mono, w0, fake_mono + 1000, i30, 1026);
	genlock_fifo_relabel_tick(&src, fake_mono + i30, w0 + i30 + (uint64_t)step, fake_mono + i30 + 1000, i30,
				  1026);
	for (size_t k = 0; k < 4; k++)
		printf("queued %zu %" PRIu64 "\n", k, frames[k].timestamp);
	printf("boundary %" PRIu64 " rx %" PRIu64 "\n", src.genlock_locked_next_boundary_ns, src.genlock_rx_last_ts);
	/* a late old-epoch arrival, then the sender's first new-epoch frame */
	struct obs_source_frame late = {{w0}};
	receive(&src, &late);
	struct obs_source_frame stepped = {{w0 + i30 + (uint64_t)step}};
	receive(&src, &stepped);
	printf("late %" PRIu64 " stepped %" PRIu64 " relabelled %" PRIu64 " old %d\n", late.timestamp,
	       stepped.timestamp, src.genlock_relabel.relabelled, src.genlock_relabel.arrival.old_epoch ? 1 : 0);
	/* the same booking again changes nothing */
	genlock_fifo_relabel_tick(&src, fake_mono + 2 * i30, w0 + 2 * i30 + (uint64_t)step, fake_mono + 2 * i30 + 1000,
				  i30, 1026);
	printf("again %" PRIu64 "\n", frames[0].timestamp);
	return 0;
}}
"#,
        glue = lift_glue(&raw),
        receive = lift_receive(&raw)
    );
    let dir = std::env::temp_dir().join(format!(
        "genlock_fifo_relabel_wiring_1372-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("scratch dir");
    let c = dir.join("glue.c");
    let bin = dir.join("glue.bin");
    fs::write(&c, harness).expect("write the lift");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg("-I")
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&c)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1372: could not run the C compiler `{cc}` ({e}); this gate must FAIL, not skip")
        });
    assert!(
        out.status.success(),
        "issue 1372: the lifted relabel glue does NOT COMPILE under -Wall -Wextra -Wconversion \
         -Wformat=2 -Werror:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run the lift");
    let _ = fs::remove_dir_all(&dir);
    assert!(run.status.success(), "the lifted glue exited non-zero");
    let text = String::from_utf8(run.stdout).expect("utf-8");
    let w0: u64 = 1_791_338_400_000_000_000;
    let i30: u64 = 33_333_333;
    let step: u64 = 1_600_000_000;
    let expect = |line: String| {
        assert!(
            text.lines().any(|l| l == line),
            "issue 1372: the lifted glue did not print `{line}`:\n{text}"
        );
    };
    for k in 0..4u64 {
        expect(format!("queued {k} {}", w0 - (4 - k) * i30 + step));
    }
    expect(format!(
        "boundary {} rx {}",
        w0 - 4 * i30 + step,
        w0 - i30 + step
    ));
    expect(format!(
        "late {} stepped {} relabelled 5 old 0",
        w0 + step,
        w0 + i30 + step
    ));
    expect(format!("again {}", w0 - 4 * i30 + step));
    assert!(
        text.contains("genlock-fifo-relabel: the wall clock stepped +1600.000 ms -- booked step 1")
            && text.contains(
                "genlock-fifo-relabel 'NDI 2ME PGM': step +1600.000 ms -- relabelled 4 of 4 \
                 queued frame(s), boundary_moved=1 arrivals=judged window_ms=1026"
            ),
        "issue 1372: the relabel log lines changed:\n{text}"
    );
    assert_eq!(
        text.matches("genlock-fifo-relabel '").count(),
        1,
        "one per-source line per booking:\n{text}"
    );
}
