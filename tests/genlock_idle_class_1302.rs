//! Issue 1302 — the genlock LOCK indicator's fast first idle classification: an input that just
//! (re)connected, was first seen or had its received counter reset is UNCLASSIFIED and contributes
//! nothing until it proves a live rate (>= 60 frames over >= 5 s) or the full #1341 window classifies
//! it.
//!
//! Before it, a connected input counted as live until its ring spanned 54 s. A keep-alive-only
//! SongPlayer input (one frame per ~11 s, a FIFO relock on each) therefore fed its relocks into
//! `recent_event` for ~54 s after every reconnect and after every OBS start (ROZHODNUTÉ 6028553391,
//! design 6028838843). Two executable gates here:
//!
//! 1. **C-vs-Rust parity** of the rule: the window constants + `genlock_input_idle_class` are lifted
//!    VERBATIM out of `GenlockLockState.hpp`, compiled with `cc`, and compared with
//!    [`camera_box::genlock_lock_state::input_idle_class`] and its constants over a grid of spans,
//!    frame deltas and previous classes.
//! 2. **A widget-shaped replay on the shipped bytes**: the widget's ring tick
//!    (`genlock_idle_classify_tick`) and its recent-event tick, both in `GenlockRecentEvents.cpp`,
//!    compiled as shipped, run in the widget's order over scripted 1 Hz scenarios, and the C
//!    `genlock_decide_lock_state` grades every tick. Each tick's classes are checked against a
//!    reference ring built on the Rust authority, and each scenario against its own hand-written
//!    expectation: a keep-alive input that reattaches never DEGRADES the box, a live input's relock
//!    after its first 5 s DEGRADES for exactly 60 s.
//!
//! Both FAIL LOUDLY rather than skip when no compiler is present.

use camera_box::genlock_lock_state::{
    input_idle_class, InputIdleClass, GENLOCK_IDLE_FAST_MIN_FRAMES, GENLOCK_IDLE_FAST_SPAN_MS,
    GENLOCK_IDLE_FULL_SPAN_MS, GENLOCK_IDLE_INPUT_MIN_FRAMES, GENLOCK_IDLE_WINDOW_MS,
};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const HEADER: &str = "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp";
const WIDGETS: &str = "vendor/obs-studio/frontend/widgets";
const RECENT_CPP: &str = "vendor/obs-studio/frontend/widgets/GenlockRecentEvents.cpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    let path = repo(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn scratch(tag: &str) -> PathBuf {
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("genlock_idle_class_1302_{tag}"));
    fs::create_dir_all(&dir).expect("create the scratch dir");
    dir
}

/// Compile `src` (plus the shipped `extra` sources) with `compiler` + `flags` and return its stdout.
/// Panics (never skips) when the compiler is missing or the program fails.
fn build_and_run(
    compiler: &str,
    flags: &[&str],
    extra: &[PathBuf],
    dir: &Path,
    name: &str,
    ext: &str,
    src: &str,
) -> String {
    let file = dir.join(format!("{name}.{ext}"));
    let bin = dir.join(format!("{name}.bin"));
    fs::write(&file, src).expect("write the harness");
    let out = Command::new(compiler)
        .args(flags)
        .arg(&file)
        .args(extra)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1302: could not run `{compiler}` ({e}). This gate compiles the vendored code to \
                 prove it computes what the Rust authority does; it must FAIL rather than skip when \
                 the toolchain is absent. Install a C/C++ compiler or set CC / CXX."
            )
        });
    assert!(
        out.status.success(),
        "issue 1302: the harness `{name}` does NOT COMPILE under {flags:?}:\n--- stderr ---\n{}\n--- harness ---\n{src}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().unwrap_or_else(|e| {
        panic!("issue 1302: the compiled harness `{name}` failed to start: {e}")
    });
    assert!(
        run.status.success(),
        "issue 1302: harness `{name}` exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout).expect("harness stdout is utf-8")
}

/// Slice `[start of `first`, end of the function whose signature is `last`]` out of the header; both
/// anchors must occur exactly once (an anchor quoted in a comment would splice the lift).
fn lift(first: &str, last: &str) -> String {
    let src = read(HEADER);
    for anchor in [first, last] {
        assert_eq!(
            src.matches(anchor).count(),
            1,
            "issue 1302: `{anchor}` must occur exactly once in {HEADER} (the lift anchors on it)"
        );
    }
    let start = src.find(first).expect("first anchor");
    let func = src.find(last).expect("last anchor");
    assert!(
        func >= start,
        "issue 1302: the idle block of {HEADER} is no longer contiguous"
    );
    let end = src[func..]
        .find("\n}\n")
        .map(|i| func + i + 3)
        .unwrap_or_else(|| panic!("issue 1302: `{last}` has no closing brace in {HEADER}"));
    src[start..end].to_string()
}

fn class_of(code: u8) -> InputIdleClass {
    match code {
        0 => InputIdleClass::Unclassified,
        1 => InputIdleClass::Live,
        2 => InputIdleClass::Idle,
        other => panic!("issue 1302: not a class code: {other}"),
    }
}

// ---- 1. the rule: C mirror vs the Rust authority -----------------------------------------------

const SPANS: [i64; 13] = [
    i64::MIN,
    -1,
    0,
    1,
    4_999,
    5_000,
    5_001,
    30_000,
    53_999,
    54_000,
    54_001,
    60_000,
    i64::MAX,
];
const DELTAS: [u64; 8] = [0, 1, 5, 59, 60, 61, 1_400, u64::MAX];

#[test]
fn c_input_idle_class_and_its_constants_match_the_rust_authority_1302() {
    let block = lift(
        "#define GENLOCK_IDLE_WINDOW_MS",
        "static inline genlock_input_idle_class_t genlock_input_idle_class(",
    );
    let fmt_i = |v: &[i64]| {
        v.iter()
            .map(|x| {
                if *x == i64::MIN {
                    "INT64_MIN".to_string()
                } else {
                    format!("INT64_C({x})")
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    let fmt_u = |v: &[u64]| {
        v.iter()
            .map(|x| format!("UINT64_C({x})"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut c = String::from("#include <stdio.h>\n#include <stdint.h>\n#include <inttypes.h>\n");
    c.push_str(&block);
    c.push_str(&format!(
        "\nstatic const int64_t SPANS[] = {{{}}};\nstatic const uint64_t DELTAS[] = {{{}}};\n",
        fmt_i(&SPANS),
        fmt_u(&DELTAS)
    ));
    c.push_str(
        r#"int main(void)
{
	printf("%" PRId64 " %" PRId64 " %" PRIu64 " %" PRId64 " %" PRIu64 "\n", (int64_t)GENLOCK_IDLE_WINDOW_MS,
	       (int64_t)GENLOCK_IDLE_FULL_SPAN_MS, (uint64_t)GENLOCK_IDLE_INPUT_MIN_FRAMES,
	       (int64_t)GENLOCK_IDLE_FAST_SPAN_MS, (uint64_t)GENLOCK_IDLE_FAST_MIN_FRAMES);
	for (size_t s = 0; s < sizeof(SPANS) / sizeof(SPANS[0]); s++)
		for (size_t d = 0; d < sizeof(DELTAS) / sizeof(DELTAS[0]); d++)
			for (int p = 0; p < 3; p++)
				printf("%d\n", (int)genlock_input_idle_class(SPANS[s], DELTAS[d], (genlock_input_idle_class_t)p));
	return 0;
}
"#,
    );
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let stdout = build_and_run(
        &cc,
        &[
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Werror",
            "-O1",
        ],
        &[],
        &scratch("parity"),
        "idle_class",
        "c",
        &c,
    );
    let mut lines = stdout.lines();
    let consts = lines.next().expect("the constants line");
    assert_eq!(
        consts,
        format!(
            "{GENLOCK_IDLE_WINDOW_MS} {GENLOCK_IDLE_FULL_SPAN_MS} {GENLOCK_IDLE_INPUT_MIN_FRAMES} \
             {GENLOCK_IDLE_FAST_SPAN_MS} {GENLOCK_IDLE_FAST_MIN_FRAMES}"
        ),
        "issue 1302: the header's idle constants DIVERGED from the Rust authority"
    );
    let got: Vec<u8> = lines.map(|l| l.trim().parse().expect("a code")).collect();
    let mut want = Vec::new();
    for s in SPANS {
        for d in DELTAS {
            for p in 0..3u8 {
                want.push((s, d, p));
            }
        }
    }
    assert_eq!(got.len(), want.len(), "issue 1302: one line per vector");
    let mut diffs = Vec::new();
    for ((s, d, p), c_code) in want.iter().zip(&got) {
        let rs = input_idle_class(*s, *d, class_of(*p)).code();
        if rs != *c_code {
            diffs.push(format!(
                "  span_ms={s} delta_frames={d} prev={p} -> C {c_code}, Rust {rs}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1302: the vendored C genlock_input_idle_class DIVERGED from the Rust authority on {} of {} vectors:\n{}",
        diffs.len(),
        want.len(),
        diffs.join("\n")
    );
}

// ---- 2. the widget-shaped replay on the shipped bytes ------------------------------------------

/// One input as the widget's scan reports it on one tick.
#[derive(Clone, Copy)]
struct Row {
    name: &'static str,
    connected: bool,
    locked: bool,
    frames: u64,
    relocks: u64,
}

fn row(name: &'static str, frames: u64, relocks: u64) -> Row {
    Row {
        name,
        connected: true,
        locked: true,
        frames,
        relocks,
    }
}

fn gone(name: &'static str) -> Row {
    Row {
        connected: false,
        locked: false,
        ..row(name, 0, 0)
    }
}

struct Scenario {
    name: &'static str,
    /// one entry per 1 Hz tick; tick i runs at i * 1000 ms, plus the stall below once past it
    ticks: Vec<Vec<Row>>,
    /// `(k, ms)`: the widget's timer stalled `ms` between tick `k` and tick `k + 1`
    stall: Option<(usize, i64)>,
}

impl Scenario {
    /// The monotonic ms of tick `k`.
    fn at(&self, k: usize) -> i64 {
        let extra = match self.stall {
            Some((after, ms)) if k > after => ms,
            _ => 0,
        };
        k as i64 * 1000 + extra
    }
}

/// The reference model of the widget's ring tick, built on the Rust authority.
#[derive(Default)]
struct RingReference {
    inputs: BTreeMap<String, (VecDeque<(i64, u64)>, InputIdleClass)>,
}

impl RingReference {
    fn tick(&mut self, now_ms: i64, rows: &[Row]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in rows {
            if !r.connected {
                out.push(InputIdleClass::Unclassified.code());
                continue;
            }
            let (ring, class) = self
                .inputs
                .entry(r.name.to_string())
                .or_insert((VecDeque::new(), InputIdleClass::Unclassified));
            if ring.back().is_some_and(|&(_, f)| r.frames < f) {
                ring.clear();
                *class = InputIdleClass::Unclassified;
            }
            ring.push_back((now_ms, r.frames));
            while ring.len() > 1 && now_ms - ring.front().expect("ring").0 > GENLOCK_IDLE_WINDOW_MS
            {
                ring.pop_front();
            }
            let (t0, f0) = *ring.front().expect("ring");
            let (t1, f1) = *ring.back().expect("ring");
            *class = input_idle_class(t1 - t0, f1 - f0, *class);
            out.push(class.code());
        }
        self.inputs
            .retain(|name, _| rows.iter().any(|r| r.connected && r.name == name.as_str()));
        out
    }
}

/// The widget's two ticks in its order + a driver that replays every scenario and prints one line
/// per tick: `scenario|tick|classes|new_events|recent|top_name|state|reason`.
fn run_replay(scenarios: &[Scenario]) -> Vec<Vec<String>> {
    let mut src = String::from(
        "#include <cstdint>\n#include <cstdio>\n#include <string>\n#include <vector>\n\
         #include \"GenlockLockState.hpp\"\n#include \"GenlockRecentEvents.hpp\"\n",
    );
    src.push_str(
        r#"
struct Row { const char *name; int connected, locked; uint64_t frames, relocks; };
static void emit(const char *sc, int tick, int64_t now_ms, GenlockIdleClassifier &idle,
		 GenlockRecentEvents &ev, const std::vector<Row> &rows)
{
	std::vector<GenlockRxInput> rx;
	for (const Row &r : rows) {
		GenlockRxInput in;
		in.name = r.name;
		in.connected = r.connected != 0;
		in.frames_received = r.frames;
		rx.push_back(in);
	}
	/* the widget's order: classify, then the idle path, then the recent-event tick, then decide */
	const std::vector<int> classes = genlock_idle_classify_tick(idle, now_ms, rx);
	genlock_lock_facets_t f = {};
	std::vector<GenlockPhaseInput> in;
	std::string cls;
	for (size_t i = 0; i < rows.size(); i++) {
		const Row &r = rows[i];
		const bool is_idle = r.connected && classes[i] != GENLOCK_INPUT_LIVE;
		f.n_inputs++;
		if (!r.connected)
			f.n_absent++;
		else if (is_idle)
			f.n_idle++;
		else if (r.locked)
			f.n_locked++;
		GenlockPhaseInput p;
		p.name = r.name;
		p.connected = r.connected != 0;
		p.idle = is_idle;
		p.relocks = r.relocks;
		in.push_back(p);
		if (i)
			cls += ",";
		cls += std::to_string(classes[i]);
	}
	const GenlockRecentEventTick t = genlock_recent_events_tick(ev, now_ms, 60000, in);
	f.recent_event = t.recent_event ? 1 : 0;
	f.clock_present = 1;
	f.clock_locked = 1;
	genlock_lock_reason_t reason;
	const genlock_lock_state_t state = genlock_decide_lock_state(&f, &reason);
	printf("%s|%d|%s|%llu|%d|%s|%d|%d\n", sc, tick, cls.c_str(), (unsigned long long)t.new_events,
	       t.recent_event ? 1 : 0, t.top_name.c_str(), (int)state, (int)reason);
}
int main()
{
"#,
    );
    for sc in scenarios {
        src.push_str("\t{\n\t\tGenlockIdleClassifier idle;\n\t\tGenlockRecentEvents ev;\n");
        for (i, rows) in sc.ticks.iter().enumerate() {
            let items: Vec<String> = rows
                .iter()
                .map(|r| {
                    format!(
                        "{{\"{}\",{},{},{}ULL,{}ULL}}",
                        r.name, r.connected as u8, r.locked as u8, r.frames, r.relocks
                    )
                })
                .collect();
            src.push_str(&format!(
                "\t\temit(\"{}\", {i}, {}, idle, ev, std::vector<Row>{{{}}});\n",
                sc.name,
                sc.at(i),
                items.join(",")
            ));
        }
        src.push_str("\t}\n");
    }
    src.push_str("\treturn 0;\n}\n");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let include = format!("-I{}", repo(WIDGETS).display());
    let stdout = build_and_run(
        &cxx,
        &["-std=c++17", "-Wall", "-Wextra", "-Werror", "-O1", &include],
        &[repo(RECENT_CPP)],
        &scratch("replay"),
        "idle_class_replay",
        "cpp",
        &src,
    );
    stdout
        .lines()
        .map(|l| l.split('|').map(str::to_string).collect())
        .collect()
}

const LOCKED: &str = "2";
const DEGRADED: &str = "1";
const REASON_RECENT_EVENT: &str = "6";

/// A live 60 fps program input (its relocks never change).
fn pgm(t: u64) -> Row {
    row("NDI 2ME PGM", 500_000 + 60 * t, 3)
}

/// A keep-alive SongPlayer input `k` ticks after its (re)connect: one frame and one FIFO relock per
/// ~11 s.
fn keep_alive(k: u64) -> Row {
    row("sp-song", 2_000 + k / 11, 40 + k / 11)
}

fn scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();

    // OBS start: every input is a first sight. A live program input, a 30 fps camera that relocks at
    // tick 2 (still UNCLASSIFIED, never counted), and a keep-alive input relocking every ~11 s.
    out.push(Scenario {
        name: "obs_start",
        ticks: (0..70u64)
            .map(|t| {
                vec![
                    pgm(t),
                    row("NDI cam3", 30 * t, if t >= 2 { 8 } else { 7 }),
                    keep_alive(t),
                ]
            })
            .collect(),
        stall: None,
    });

    // A keep-alive input that has been IDLE for 70 s, vanishes for 5 s and reattaches: its relocks
    // after the reattach never count, the box stays LOCKED throughout.
    let mut t: Vec<Vec<Row>> = (0..70u64).map(|k| vec![pgm(k), keep_alive(k)]).collect();
    t.extend((70..75u64).map(|k| vec![pgm(k), gone("sp-song")]));
    t.extend((75..165u64).map(|k| vec![pgm(k), keep_alive(k)]));
    out.push(Scenario {
        name: "keepalive_reattach",
        ticks: t,
        stall: None,
    });

    // A live camera reattaches at tick 75: it relocks at tick 77 (UNCLASSIFIED, never counted), is
    // LIVE from tick 80 (its baseline), and really relocks at tick 82: DEGRADED for exactly 60 s.
    let cam = |k: u64, relocks: u64| row("NDI cam7", 900_000 + 60 * k, relocks);
    let mut t: Vec<Vec<Row>> = (0..70u64).map(|k| vec![pgm(k), cam(k, 7)]).collect();
    t.extend((70..75u64).map(|k| vec![pgm(k), gone("NDI cam7")]));
    t.extend((75..146u64).map(|k| {
        let relocks = match k {
            ..=76 => 7,
            77..=81 => 8,
            _ => 9,
        };
        vec![pgm(k), cam(k, relocks)]
    }));
    out.push(Scenario {
        name: "live_reattach",
        ticks: t,
        stall: None,
    });

    // The source is recreated with no disconnect: its received counter restarts at 0 (tick 70). A
    // relock at tick 72 (UNCLASSIFIED) never counts; LIVE from tick 75; a relock at tick 80 DEGRADES.
    let mut t: Vec<Vec<Row>> = (0..70u64)
        .map(|k| vec![pgm(k), row("CG-obs", 60 * k, 5)])
        .collect();
    t.extend((70..145u64).map(|k| {
        let relocks = match k {
            ..=71 => 0,
            72..=79 => 1,
            _ => 2,
        };
        vec![pgm(k), row("CG-obs", 60 * (k - 70), relocks)]
    }));
    out.push(Scenario {
        name: "counter_reset",
        ticks: t,
        stall: None,
    });

    // A 5 fps still source: UNCLASSIFIED until it has delivered 60 frames (tick 12); a relock at tick
    // 8 never counts, one at tick 20 DEGRADES.
    out.push(Scenario {
        name: "slow_source",
        ticks: (0..85u64)
            .map(|k| {
                let relocks = match k {
                    ..=7 => 1,
                    8..=19 => 2,
                    _ => 3,
                };
                vec![pgm(k), row("still-5fps", 5 * k, relocks)]
            })
            .collect(),
        stall: None,
    });
    // The widget's 1 Hz timer stalls for 70 s after tick 69 (a blocked UI thread): every ring is
    // pruned to its newest sample. A decided class holds through the short ring -- the keep-alive
    // input stays IDLE, the camera stays LIVE -- so the camera's relock right after the stall still
    // DEGRADES and the keep-alive input never turns LIVE.
    let stall_ms = 70_000;
    let at = |k: u64| k * 1000 + if k > 69 { stall_ms as u64 } else { 0 };
    out.push(Scenario {
        name: "widget_stall",
        ticks: (0..80u64)
            .map(|k| {
                let s = at(k) / 1000;
                vec![
                    pgm(s),
                    keep_alive(s),
                    row("NDI cam5", 60 * s, if k >= 71 { 4 } else { 3 }),
                ]
            })
            .collect(),
        stall: Some((69, stall_ms)),
    });
    out
}

#[test]
fn the_widget_ticks_match_the_reference_and_only_a_live_input_degrades_1302() {
    let scs = scenarios();
    let lines = run_replay(&scs);
    let total: usize = scs.iter().map(|s| s.ticks.len()).sum();
    assert_eq!(lines.len(), total, "issue 1302: one replay line per tick");

    // every tick: the shipped ring tick classifies exactly as the Rust-authority reference
    let mut by_scenario: BTreeMap<&str, Vec<&Vec<String>>> = BTreeMap::new();
    let mut i = 0;
    let mut diffs = Vec::new();
    for sc in &scs {
        let mut reference = RingReference::default();
        for (k, rows) in sc.ticks.iter().enumerate() {
            let line = &lines[i];
            i += 1;
            assert_eq!(line.len(), 8, "issue 1302: malformed replay line {line:?}");
            assert_eq!(line[0], sc.name);
            assert_eq!(line[1], k.to_string());
            let want: Vec<String> = reference
                .tick(sc.at(k), rows)
                .iter()
                .map(u8::to_string)
                .collect();
            if line[2] != want.join(",") {
                diffs.push(format!(
                    "  {} tick {k}: C++ classes {}, reference {}",
                    sc.name,
                    line[2],
                    want.join(",")
                ));
            }
            by_scenario.entry(sc.name).or_default().push(line);
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1302: the widget's genlock_idle_classify_tick DIVERGED from the Rust-authority reference:\n{}",
        diffs.join("\n")
    );

    let state = |sc: &str, k: usize| by_scenario[sc][k][6].clone();
    let reason = |sc: &str, k: usize| by_scenario[sc][k][7].clone();
    let field = |sc: &str, k: usize, f: usize| by_scenario[sc][k][f].clone();
    let class = |sc: &str, k: usize, input: usize| {
        by_scenario[sc][k][2]
            .split(',')
            .nth(input)
            .expect("a class per input")
            .to_string()
    };
    let all_locked = |sc: &str| {
        for k in 0..by_scenario[sc].len() {
            assert_eq!(
                state(sc, k),
                LOCKED,
                "issue 1302: {sc} tick {k} must stay LOCKED (reason {}, classes {})",
                reason(sc, k),
                field(sc, k, 2)
            );
            assert_eq!(field(sc, k, 3), "0", "{sc} tick {k}: no new event");
        }
    };
    let degraded_for_60_s = |sc: &str, from: usize, who: &str| {
        for k in from..from + 60 {
            assert_eq!(state(sc, k), DEGRADED, "{sc} tick {k}");
            assert_eq!(reason(sc, k), REASON_RECENT_EVENT, "{sc} tick {k}");
            assert_eq!(field(sc, k, 5), who, "{sc} tick {k}");
        }
        assert_eq!(state(sc, from + 60), LOCKED, "{sc}: 60 s after the event");
    };

    // OBS start: never DEGRADED; the live inputs turn LIVE at tick 5, the keep-alive one never does
    all_locked("obs_start");
    for k in 0..5 {
        assert_eq!(field("obs_start", k, 2), "0,0,0", "obs_start tick {k}");
    }
    for k in 5..54 {
        assert_eq!(field("obs_start", k, 2), "1,1,0", "obs_start tick {k}");
    }
    for k in 54..70 {
        assert_eq!(field("obs_start", k, 2), "1,1,2", "obs_start tick {k}");
    }

    // the keep-alive reattach: LOCKED on every one of its 165 ticks, never LIVE
    all_locked("keepalive_reattach");
    for k in 0..165 {
        assert_ne!(
            class("keepalive_reattach", k, 1),
            "1",
            "keepalive_reattach tick {k}: a keep-alive input must never contribute"
        );
    }

    // the live reattach: the UNCLASSIFIED relock is never counted, the LIVE one DEGRADES for 60 s
    for k in 0..82 {
        assert_eq!(state("live_reattach", k), LOCKED, "live_reattach tick {k}");
    }
    for k in 75..80 {
        assert_eq!(class("live_reattach", k, 1), "0", "live_reattach tick {k}");
    }
    assert_eq!(class("live_reattach", 80, 1), "1");
    assert_eq!(field("live_reattach", 82, 3), "1", "the LIVE relock counts");
    degraded_for_60_s("live_reattach", 82, "NDI cam7");

    // the counter reset re-classifies from scratch: UNCLASSIFIED ticks 70-74, LIVE from 75
    for k in 0..80 {
        assert_eq!(state("counter_reset", k), LOCKED, "counter_reset tick {k}");
    }
    for k in 70..75 {
        assert_eq!(class("counter_reset", k, 1), "0", "counter_reset tick {k}");
    }
    assert_eq!(class("counter_reset", 75, 1), "1");
    degraded_for_60_s("counter_reset", 80, "CG-obs");

    // the 5 fps source: LIVE once 60 frames arrived (tick 12); only its LIVE relock counts
    for k in 0..12 {
        assert_eq!(class("slow_source", k, 1), "0", "slow_source tick {k}");
    }
    assert_eq!(class("slow_source", 12, 1), "1");
    for k in 0..20 {
        assert_eq!(state("slow_source", k), LOCKED, "slow_source tick {k}");
    }
    degraded_for_60_s("slow_source", 20, "still-5fps");

    // the widget stall: the classes hold through the pruned rings, the LIVE relock still counts
    for k in 54..80 {
        assert_eq!(class("widget_stall", k, 1), "2", "widget_stall tick {k}");
        assert_eq!(class("widget_stall", k, 2), "1", "widget_stall tick {k}");
    }
    for k in 0..71 {
        assert_eq!(state("widget_stall", k), LOCKED, "widget_stall tick {k}");
    }
    for k in 71..80 {
        assert_eq!(state("widget_stall", k), DEGRADED, "widget_stall tick {k}");
        assert_eq!(
            field("widget_stall", k, 5),
            "NDI cam5",
            "widget_stall tick {k}"
        );
    }
}
