//! Issue 1302 — the genlock LOCK indicator's per-input event BASELINE: a reattach never counts an
//! input's old events as new.
//!
//! The #1299 widget summed every connected input's LIFETIME `relocks + late_holds + backward_steps`
//! into one aggregate and raised `recent_event` on any rise. A reconnecting or waking input added its
//! whole total at once, so the box read DEGRADED `recent_event` for 60 s after every reattach (the
//! SongPlayer A/V post-deploy gate, songplayer issue 221). Two executable gates here:
//!
//! 1. **C-vs-Rust parity** of the per-input rule: `genlock_input_new_phase_events` is lifted VERBATIM
//!    out of `GenlockLockState.hpp`, compiled with `cc`, and compared with
//!    [`camera_box::genlock_lock_state::input_new_phase_events`] over every flag combination crossed
//!    with a spread of totals (equal, rise, backward, both extremes).
//! 2. **A widget-shaped replay on the shipped bytes**: the widget's tick
//!    (`genlock_recent_events_tick` + its two structs, lifted verbatim from `OBSBasicStatusBar.cpp`,
//!    with the real `GenlockRecentEvents.hpp` and `GenlockLockState.hpp`) runs under `c++` over
//!    scripted 1 Hz scenarios, and the C `genlock_decide_lock_state` grades every tick. Each tick is
//!    checked against a reference model built on the Rust authority, and each scenario against its
//!    own hand-written expectation: an input with 40 lifetime relocks that reattaches stays LOCKED,
//!    a real event after the attach still DEGRADES for exactly 60 s.
//!
//! Both FAIL LOUDLY rather than skip when no compiler is present (the project's test-strictness
//! rule: a parity gate that silently passes without running is worse than none).

use camera_box::genlock_lock_state::{input_new_phase_events, InputEventCounts, PhaseEventSample};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const HEADER: &str = "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp";
const STATE_HEADER: &str = "vendor/obs-studio/frontend/widgets/GenlockRecentEvents.hpp";
const STATUSBAR_CPP: &str = "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.cpp";
/// The widget's `GENLOCK_RECENT_EVENT_WINDOW_MS` (pinned by `tests/genlock_lock_json_guards.rs`).
const WINDOW_MS: i64 = 60_000;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    let path = repo(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("genlock_phase_baseline_1302_{tag}"));
    fs::create_dir_all(&dir).expect("create the scratch dir");
    dir
}

/// Compile `src` with `compiler` + `flags` into `dir/<name>` and return its stdout. Panics (never
/// skips) when the compiler is missing or the program fails.
fn build_and_run(
    compiler: &str,
    flags: &[&str],
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
        "issue 1302: the lifted harness `{name}` does NOT COMPILE under {flags:?}:\n--- stderr ---\n{}\n--- harness ---\n{src}",
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

/// Slice `[start of `first`, end of the function whose signature is `last`]` out of `src`; both
/// anchors must occur exactly once (an anchor quoted in a comment would splice the lift).
fn lift(src: &str, file: &str, first: &str, last: &str) -> String {
    for anchor in [first, last] {
        assert_eq!(
            src.matches(anchor).count(),
            1,
            "issue 1302: `{anchor}` must occur exactly once in {file} (the lift anchors on it)"
        );
    }
    let start = src.find(first).expect("first anchor");
    let func = src.find(last).expect("last anchor");
    assert!(
        func >= start,
        "issue 1302: the lifted block of {file} is no longer contiguous"
    );
    let end = src[func..]
        .find("\n}\n")
        .map(|i| func + i + 3)
        .unwrap_or_else(|| panic!("issue 1302: `{last}` has no closing brace in {file}"));
    src[start..end].to_string()
}

// ---- 1. the per-input rule: C mirror vs the Rust authority ------------------------------------

#[test]
fn c_input_new_phase_events_matches_the_rust_authority_1302() {
    let sig = "static inline uint64_t genlock_input_new_phase_events(";
    let block = lift(&read(HEADER), HEADER, sig, sig);

    let big = u64::MAX;
    let totals = [0u64, 1, 3, 40, 41, 567, big - 1, big];
    let mut vs: Vec<(bool, bool, u64, bool, u64)> = Vec::new();
    for &has_prev in &[false, true] {
        for &prev_c in &[false, true] {
            for &cur_c in &[false, true] {
                for &p in &totals {
                    for &t in &totals {
                        vs.push((has_prev, prev_c, p, cur_c, t));
                    }
                }
            }
        }
    }

    let mut c = String::from("#include <stdio.h>\n#include <stdint.h>\n#include <inttypes.h>\n");
    c.push_str(&block);
    c.push_str("static const uint64_t V[][5] = {\n");
    for (h, pc, p, cc, t) in &vs {
        c.push_str(&format!(
            "  {{{}u,{}u,{p}ULL,{}u,{t}ULL}},\n",
            *h as u8, *pc as u8, *cc as u8
        ));
    }
    c.push_str("};\nint main(void){\n");
    c.push_str("  for (size_t i = 0; i < sizeof(V) / sizeof(V[0]); i++)\n");
    c.push_str("    printf(\"%\" PRIu64 \"\\n\", genlock_input_new_phase_events((int)V[i][0], (int)V[i][1], V[i][2], (int)V[i][3], V[i][4]));\n");
    c.push_str("  return 0;\n}\n");

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
        &scratch("parity"),
        "new_phase_events",
        "c",
        &c,
    );
    let c_out: Vec<u64> = stdout
        .lines()
        .map(|l| l.trim().parse().expect("a u64 per line"))
        .collect();
    assert_eq!(
        c_out.len(),
        vs.len(),
        "issue 1302: one output line per vector"
    );

    let mut diffs = Vec::new();
    for ((h, pc, p, cc, t), got_c) in vs.iter().zip(&c_out) {
        let prev = h.then_some(PhaseEventSample {
            total: *p,
            contributing: *pc,
        });
        let cur = PhaseEventSample {
            total: *t,
            contributing: *cc,
        };
        let got_rs = input_new_phase_events(prev, cur);
        if got_rs != *got_c {
            diffs.push(format!(
                "  has_prev={h} prev_contributing={pc} prev_total={p} contributing={cc} total={t} -> C {got_c}, Rust {got_rs}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1302: the vendored C genlock_input_new_phase_events DIVERGED from the Rust authority on {} of {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}

// ---- 2. the widget-shaped replay on the shipped bytes ------------------------------------------

/// One input as the widget's scan reports it on one tick.
#[derive(Clone, Copy)]
struct Row {
    name: &'static str,
    connected: bool,
    idle: bool,
    locked: bool,
    relocks: u64,
    late_holds: u64,
    backward_steps: u64,
}

/// A connected, locked, live input with this lifetime relock total.
fn live(name: &'static str, relocks: u64) -> Row {
    Row {
        name,
        connected: true,
        idle: false,
        locked: true,
        relocks,
        late_holds: 0,
        backward_steps: 0,
    }
}

/// The same input with no NDI connection (its sender is gone).
fn absent(name: &'static str, relocks: u64) -> Row {
    Row {
        connected: false,
        locked: false,
        ..live(name, relocks)
    }
}

/// The same input connected but keep-alive-only (#1341 idle; the widget drops it from n_locked).
fn idle(name: &'static str, relocks: u64) -> Row {
    Row {
        idle: true,
        locked: false,
        ..live(name, relocks)
    }
}

struct Scenario {
    name: &'static str,
    /// one entry per 1 Hz tick; tick i runs at i * 1000 ms
    ticks: Vec<Vec<Row>>,
}

/// What one tick produced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TickOut {
    new_events: u64,
    recent_event: bool,
    top_name: String,
    top_events: u64,
    remembered: usize,
}

/// The reference model of the widget's tick, built on the Rust authority (`input_new_phase_events`).
#[derive(Default)]
struct Reference {
    inputs: BTreeMap<String, (PhaseEventSample, VecDeque<(i64, u64)>)>,
    last_event_ms: i64,
}

impl Reference {
    fn tick(&mut self, now_ms: i64, rows: &[Row]) -> TickOut {
        let mut out = TickOut {
            new_events: 0,
            recent_event: false,
            top_name: String::new(),
            top_events: 0,
            remembered: 0,
        };
        for r in rows {
            let cur = PhaseEventSample::of(&InputEventCounts {
                connected: r.connected,
                idle: r.idle,
                relocks: r.relocks,
                late_holds: r.late_holds,
                backward_steps: r.backward_steps,
            });
            let prev = self.inputs.get(r.name).map(|(s, _)| *s);
            let fresh = input_new_phase_events(prev, cur);
            let entry = self
                .inputs
                .entry(r.name.to_string())
                .or_insert((cur, VecDeque::new()));
            entry.0 = cur;
            if fresh > 0 {
                entry.1.push_back((now_ms, fresh));
                out.new_events = out.new_events.saturating_add(fresh);
            }
            while entry
                .1
                .front()
                .is_some_and(|(t, _)| now_ms - t >= WINDOW_MS)
            {
                entry.1.pop_front();
            }
            let windowed = entry.1.iter().fold(0u64, |a, (_, e)| a.saturating_add(*e));
            if windowed > out.top_events {
                out.top_events = windowed;
                out.top_name = r.name.to_string();
            }
        }
        self.inputs
            .retain(|name, _| rows.iter().any(|r| r.name == name.as_str()));
        if out.new_events > 0 {
            self.last_event_ms = now_ms;
        }
        out.recent_event = self.last_event_ms >= 0 && now_ms - self.last_event_ms < WINDOW_MS;
        out.remembered = self.inputs.len();
        out
    }
}

/// The lifted widget tick + a driver that replays every scenario and prints one line per tick:
/// `scenario|tick|new|recent|top_name|top_events|remembered|state|reason`.
fn run_replay(scenarios: &[Scenario]) -> Vec<Vec<String>> {
    let cpp = read(STATUSBAR_CPP);
    let block = lift(
        &cpp,
        STATUSBAR_CPP,
        "struct GenlockPhaseInput {",
        "GenlockRecentEventTick genlock_recent_events_tick(",
    );
    let mut src = String::from(
        "#include <cstdint>\n#include <cstdio>\n#include <set>\n#include <string>\n#include <vector>\n",
    );
    src.push_str(&format!("#include \"{}\"\n", repo(HEADER).display()));
    src.push_str(&format!("#include \"{}\"\n", repo(STATE_HEADER).display()));
    src.push_str(&block);
    src.push_str(
        r#"
struct Row { const char *name; int connected, idle, locked; uint64_t relocks, late_holds, backward_steps; };
static void emit(const char *sc, int tick, GenlockRecentEvents &st, const std::vector<Row> &rows)
{
	std::vector<GenlockPhaseInput> in;
	genlock_lock_facets_t f = {};
	for (const Row &r : rows) {
		GenlockPhaseInput p;
		p.name = r.name;
		p.connected = r.connected != 0;
		p.idle = r.idle != 0;
		p.relocks = r.relocks;
		p.late_holds = r.late_holds;
		p.backward_steps = r.backward_steps;
		in.push_back(p);
		f.n_inputs++;
		if (r.locked && r.connected && !r.idle)
			f.n_locked++;
		if (!r.connected)
			f.n_absent++;
		else if (r.idle)
			f.n_idle++;
	}
	const GenlockRecentEventTick t = genlock_recent_events_tick(st, (int64_t)tick * 1000, 60000, in);
	f.recent_event = t.recent_event ? 1 : 0;
	f.clock_present = 1;
	f.clock_locked = 1;
	genlock_lock_reason_t reason;
	const genlock_lock_state_t state = genlock_decide_lock_state(&f, &reason);
	printf("%s|%d|%llu|%d|%s|%llu|%zu|%d|%d\n", sc, tick, (unsigned long long)t.new_events,
	       t.recent_event ? 1 : 0, t.top_name.c_str(), (unsigned long long)t.top_events, st.inputs.size(),
	       (int)state, (int)reason);
}
int main()
{
"#,
    );
    for sc in scenarios {
        src.push_str("\t{\n\t\tGenlockRecentEvents st;\n");
        for (i, rows) in sc.ticks.iter().enumerate() {
            let items: Vec<String> = rows
                .iter()
                .map(|r| {
                    format!(
                        "{{\"{}\",{},{},{},{}ULL,{}ULL,{}ULL}}",
                        r.name,
                        r.connected as u8,
                        r.idle as u8,
                        r.locked as u8,
                        r.relocks,
                        r.late_holds,
                        r.backward_steps
                    )
                })
                .collect();
            src.push_str(&format!(
                "\t\temit(\"{}\", {i}, st, std::vector<Row>{{{}}});\n",
                sc.name,
                items.join(",")
            ));
        }
        src.push_str("\t}\n");
    }
    src.push_str("\treturn 0;\n}\n");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let stdout = build_and_run(
        &cxx,
        &["-std=c++17", "-Wall", "-Wextra", "-Werror", "-O1"],
        &scratch("replay"),
        "recent_events_replay",
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

/// `n` identical ticks.
fn repeat(n: usize, rows: Vec<Row>) -> Vec<Vec<Row>> {
    (0..n).map(|_| rows.clone()).collect()
}

fn scenarios() -> Vec<Scenario> {
    let pgm = "NDI 2ME PGM";
    let probe = "sp-probe";
    let mut out = Vec::new();

    // The SongPlayer probe: 40 lifetime relocks, its sender stops for 5 s and comes back. The
    // reattach must never read as 40 new events.
    let mut t = repeat(5, vec![live(pgm, 3), live(probe, 40)]);
    t.extend(repeat(5, vec![live(pgm, 3), absent(probe, 40)]));
    t.extend(repeat(70, vec![live(pgm, 3), live(probe, 40)]));
    out.push(Scenario {
        name: "reattach_40",
        ticks: t,
    });

    // The same input reattaches, then really relocks once at tick 12: DEGRADED for exactly 60 s.
    let mut t = repeat(5, vec![live(pgm, 3), live(probe, 40)]);
    t.extend(repeat(5, vec![live(pgm, 3), absent(probe, 40)]));
    t.extend(repeat(2, vec![live(pgm, 3), live(probe, 40)]));
    t.extend(repeat(64, vec![live(pgm, 3), live(probe, 41)]));
    out.push(Scenario {
        name: "event_after_attach",
        ticks: t,
    });

    // A keep-alive-only playlist input relocks on every keep-alive frame while idle (it never
    // counts), then the song starts: the wake must not count its 60 idle relocks.
    let song = "sp-song";
    let mut t: Vec<Vec<Row>> = (0..10)
        .map(|i| vec![live(pgm, 3), idle(song, 50 + i)])
        .collect();
    t.extend(repeat(70, vec![live(pgm, 3), live(song, 60)]));
    out.push(Scenario {
        name: "wake_from_idle",
        ticks: t,
    });

    // An input that leaves the scan (its source was removed) is forgotten; it returns with 15 more
    // relocks collected while away: first sight, never new events.
    let cg = "CG-obs";
    let mut t = repeat(3, vec![live(pgm, 3), live(cg, 10)]);
    t.extend(repeat(5, vec![live(pgm, 3)]));
    t.extend(repeat(5, vec![live(pgm, 3), live(cg, 25)]));
    out.push(Scenario {
        name: "vanish_and_return",
        ticks: t,
    });

    // A counter that goes backward (the source was recreated) re-baselines; the next rise counts.
    let mut t = repeat(3, vec![live(pgm, 3), live(cg, 50)]);
    t.extend(repeat(3, vec![live(pgm, 3), live(cg, 3)]));
    t.extend(repeat(3, vec![live(pgm, 3), live(cg, 5)]));
    out.push(Scenario {
        name: "backward_reset",
        ticks: t,
    });

    // The offender is the input with the most new events IN THE WINDOW, not the largest lifetime
    // total: cam7 (500 lifetime) relocks 3 times at tick 1, cg (10 lifetime) twice at 30 and twice at
    // 40; at tick 61 cam7's events have aged out.
    let cam = "NDI cam7";
    let mut t = vec![vec![live(cam, 500), live(cg, 10)]];
    t.extend(repeat(29, vec![live(cam, 503), live(cg, 10)]));
    t.extend(repeat(10, vec![live(cam, 503), live(cg, 12)]));
    t.extend(repeat(30, vec![live(cam, 503), live(cg, 14)]));
    out.push(Scenario {
        name: "offender_window",
        ticks: t,
    });

    // A tie in the window keeps the first input in scan order.
    let mut t = vec![vec![live(cam, 500), live(cg, 10)]];
    t.extend(repeat(3, vec![live(cam, 502), live(cg, 12)]));
    out.push(Scenario {
        name: "offender_tie",
        ticks: t,
    });
    out
}

#[test]
fn the_widget_tick_matches_the_reference_and_a_reattach_stays_locked_1302() {
    let scs = scenarios();
    let lines = run_replay(&scs);
    let total: usize = scs.iter().map(|s| s.ticks.len()).sum();
    assert_eq!(lines.len(), total, "issue 1302: one replay line per tick");

    // every tick: the shipped C++ computes exactly what the Rust-authority reference computes
    let mut by_scenario: BTreeMap<&str, Vec<&Vec<String>>> = BTreeMap::new();
    let mut i = 0;
    let mut diffs = Vec::new();
    for sc in &scs {
        let mut reference = Reference {
            last_event_ms: -1,
            ..Default::default()
        };
        for (k, rows) in sc.ticks.iter().enumerate() {
            let line = &lines[i];
            i += 1;
            assert_eq!(line.len(), 9, "issue 1302: malformed replay line {line:?}");
            assert_eq!(line[0], sc.name);
            assert_eq!(line[1], k.to_string());
            let want = reference.tick(k as i64 * 1000, rows);
            let got = TickOut {
                new_events: line[2].parse().expect("new events"),
                recent_event: line[3] == "1",
                top_name: line[4].clone(),
                top_events: line[5].parse().expect("top events"),
                remembered: line[6].parse().expect("remembered"),
            };
            if got != want {
                diffs.push(format!(
                    "  {} tick {k}: C++ {got:?}, reference {want:?}",
                    sc.name
                ));
            }
            by_scenario.entry(sc.name).or_default().push(line);
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1302: the widget's genlock_recent_events_tick DIVERGED from the Rust-authority reference:\n{}",
        diffs.join("\n")
    );

    let state = |sc: &str, k: usize| by_scenario[sc][k][7].clone();
    let reason = |sc: &str, k: usize| by_scenario[sc][k][8].clone();
    let field = |sc: &str, k: usize, f: usize| by_scenario[sc][k][f].clone();

    // the songplayer reattach: LOCKED on every one of its 80 ticks, no new event ever
    for k in 0..80 {
        assert_eq!(
            state("reattach_40", k),
            LOCKED,
            "issue 1302: tick {k} — an input with 40 lifetime relocks that reattaches must stay LOCKED (reason {})",
            reason("reattach_40", k)
        );
        assert_eq!(field("reattach_40", k, 2), "0");
    }

    // a real relock after the attach DEGRADES for exactly 60 s and names the input with 1 event
    for k in 0..12 {
        assert_eq!(state("event_after_attach", k), LOCKED, "tick {k}");
    }
    for k in 12..72 {
        assert_eq!(state("event_after_attach", k), DEGRADED, "tick {k}");
        assert_eq!(
            reason("event_after_attach", k),
            REASON_RECENT_EVENT,
            "tick {k}"
        );
        assert_eq!(field("event_after_attach", k, 4), "sp-probe", "tick {k}");
        assert_eq!(field("event_after_attach", k, 5), "1", "tick {k}");
    }
    for k in 72..76 {
        assert_eq!(
            state("event_after_attach", k),
            LOCKED,
            "tick {k} (60 s after the event)"
        );
    }

    // the wake from idle and the vanished input's return never read as events
    for sc in ["wake_from_idle", "vanish_and_return"] {
        for k in 0..by_scenario[sc].len() {
            assert_eq!(state(sc, k), LOCKED, "{sc} tick {k}");
        }
    }
    // the forgotten input is not remembered while it is away
    assert_eq!(field("vanish_and_return", 4, 6), "1");
    assert_eq!(field("vanish_and_return", 9, 6), "2");

    // a backward counter re-baselines silently; the next rise counts
    for k in 0..6 {
        assert_eq!(
            state("backward_reset", k),
            LOCKED,
            "backward_reset tick {k}"
        );
    }
    assert_eq!(field("backward_reset", 6, 2), "2");
    assert_eq!(state("backward_reset", 6), DEGRADED);

    // the offender is the most NEW events in the window
    assert_eq!(field("offender_window", 1, 4), "NDI cam7");
    assert_eq!(field("offender_window", 1, 5), "3");
    assert_eq!(field("offender_window", 30, 4), "NDI cam7");
    assert_eq!(field("offender_window", 40, 4), "CG-obs");
    assert_eq!(field("offender_window", 40, 5), "4");
    assert_eq!(field("offender_window", 61, 4), "CG-obs");
    assert_eq!(field("offender_window", 61, 5), "4");
    assert_eq!(field("offender_tie", 1, 4), "NDI cam7");
    assert_eq!(field("offender_tie", 1, 5), "2");
}
