//! Issue 1439 — EXECUTABLE C-vs-Rust parity gate for the obs-vban receive timeline.
//!
//! The patched obs-vban source (`vendor/obs-vban/src/vban-source.c`) asks the pure decision in
//! `vendor/obs-vban/src/vban-receive-timeline.h` for every packet's stamp. The plugin only compiles
//! in the genlock CI builds, so this gate buys the verification back on any Linux box:
//!
//! 1. It `#include`s the SHIPPED header by its absolute path (nothing is retyped) into a driver and
//!    compiles it with `cc -std=c11 -ffp-contract=off -Wall -Wextra -Wconversion -Wsign-conversion
//!    -Wformat=2 -Werror` (the fit is in doubles; no contraction keeps C and Rust bit-identical).
//! 2. It replays scripted packet sequences: two-clock senders at several rates, packet sizes and
//!    clock offsets with burst jitter, losses, late packets exactly at and one ns past their stamp,
//!    backwards and duplicate indexes, jumps of exactly two seconds and one packet more, an index
//!    wrap, Receive Buffer and format changes, resets, packets on the outlier line and one ns either
//!    side, a sender beyond the ±500 ppm clamp, a stale anchor beyond R, packets in time but off the
//!    line, the held first second, late drops and a persistent delay step.
//! 3. It requires the C decision and state after every packet to equal the Tier-0 authority
//!    `src/vban_receive_timeline.rs` exactly, the doubles compared bit for bit.
//!
//! The wiring (the source stamps from the timeline, stays on the genlock ASRC's STAMP basis, the
//! builds carry the plugin) is pinned in `tests/vban_receive_wiring_1439.rs`.
//!
//! The Rust authority is included by `#[path]`, so this file is std-only. It runs under
//! `cargo test` in CI AND standalone, with no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_receive_timeline_parity_1439.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required. Per the project's test-strictness rule this FAILS LOUDLY rather than
//! skipping when the toolchain is missing.

#[allow(dead_code)]
#[path = "../src/vban_receive_timeline.rs"]
mod vban_receive_timeline;

#[path = "support/vban_c_1439.rs"]
mod vban_c;

#[path = "support/vban_receive_scripts_1439.rs"]
mod script_set;

use script_set::{scripts, Pkt};
use std::collections::VecDeque;
use std::fmt::Write as _;
use vban_c::{compile_and_run, repo};
use vban_receive_timeline::{
    clamp_reserve_ms, ns_to_samples, round_half_away, samples_to_ns, scale_ppb, Decision, Due,
    DueDecision, Event, Reason, Timeline, DEADLINE_MARGIN_MS, FIT_WINDOW_S, FROZEN_S, JUMP_S,
    OUTLIER_MS, STALE_PACKETS,
};

const HEADER: &str = "vendor/obs-vban/src/vban-receive-timeline.h";
const MS: u64 = 1_000_000;

// ---------------------------------------------------------------------------------------------
// Traces

const CLAMP_INPUTS: [i64; 9] = [-100, -1, 0, 1, 9, 10, 40, 500, 501];
const NS_INPUTS: [(u64, u32); 6] = [
    (0, 48_000),
    (256, 48_000),
    (103, 96_000),
    (1_587_600_001, 44_100),
    (5, 0),
    (123_456_789_012, 192_000),
];
/// `ns_to_samples` (the lead-in's length): a whole second, an exact frame, one ns short of one,
/// R − one packet on the grid, and a value far past any lead-in.
const SAMPLES_INPUTS: [(u64, u32); 6] = [
    (0, 48_000),
    (1_000_000_000, 48_000),
    (20_833, 48_000),
    (20_834, 48_000),
    (34_666_667, 48_000),
    (123_456_789_012, 192_000),
];
const SCALE_INPUTS: [(u64, i64); 7] = [
    (0, 50_000),
    (1_000_000_000, 50_000),
    (1_000_000_000, -50_000),
    (999, 1_000),
    (999, -1_000),
    (28_800_000_000_000, 500_000),
    (5_333_333, -123_457),
];
/// Beyond the round limit (+-2^62), at it, NaN and a large in-range value (review round 1: a C cast of
/// an out-of-range double is undefined, Rust `as` saturates).
const ROUND_INPUTS: [f64; 15] = [
    0.0,
    0.49,
    0.5,
    2.5,
    -0.5,
    -2.5,
    -2.4,
    123_456.499_999,
    1e300,
    -1e300,
    1e19,
    4_611_686_018_427_387_904.0,
    -4_611_686_018_427_387_904.0,
    f64::NAN,
    4.0e18,
];

fn bits(x: f64) -> u64 {
    x.to_bits()
}

fn line(t: &Timeline, d: Decision) -> String {
    let b = &t.buckets[(t.last_k.rem_euclid(FIT_WINDOW_S as i64)) as usize];
    format!(
        "{} {} {} {} {} {} {} | {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} | {} {} {} {} {} {} {} | {} {} {} {} {} {} {} {} | {} {} | {} {}",
        d.event as i32,
        d.reason as i32,
        d.pos,
        d.stamp_ns,
        d.lateness_ns,
        d.late_ns,
        d.starve_ns,
        u8::from(t.anchored),
        t.rate,
        t.channels,
        t.reserve_ns,
        t.reserve_eff_ns,
        t.anchor_ns,
        t.anchor_end,
        t.last_nu,
        t.next_pos,
        t.base_stamp_ns,
        t.base_pos,
        t.step_ppb,
        t.last_k,
        t.last_intime_ns,
        u8::from(t.fit_valid),
        t.fit_ppb,
        bits(t.fit_b),
        bits(t.fit_xbar),
        bits(t.fit_ybar),
        t.phase_ns,
        t.received,
        t.lost,
        t.silence_samples,
        t.late,
        t.re_anchors,
        t.stale,
        t.last_fold_end,
        u8::from(t.fit_frozen()),
        t.frames,
        u8::from(t.emitted),
        t.emit_pos,
        u8::from(t.lead_in_pending),
        b.k,
        b.n,
        b.x0,
        b.y0,
        bits(b.su),
        bits(b.sv),
        bits(b.suu),
        bits(b.suv),
        t.stamp_at(t.next_pos + 1000),
        t.stamp_at(t.base_pos.saturating_sub(1000)),
        t.recv_mask,
        t.reordered,
    )
}

/// One playout decision and the emission state after it.
fn due_line(t: &Timeline, d: DueDecision) -> String {
    format!(
        "due {} {} {} {} {} {} {} {} | {} {} {} {} {} | {} {} {}",
        d.kind as i32,
        d.pos,
        d.frames,
        d.stamp_ns,
        d.deadline_ns,
        d.late_ns,
        d.next_ns,
        u8::from(d.toward_next),
        t.emit_pos,
        u8::from(t.lead_in_pending),
        t.lost,
        t.silence_samples,
        t.missed,
        u8::from(t.prev_audio),
        t.conceal_run,
        t.concealed
    )
}

/// What the worker saw before one decision (the coverage counters read it): the store's head, and
/// whether the slot before went out with audio and how many concealed slots ran right before it.
#[derive(Clone, Copy)]
struct Look {
    head: Option<u64>,
    prev_audio: bool,
    run: u32,
}

/// The source's two sides around one timeline, the same in both twins' drivers (ROZHODNUTÉ
/// 6347872706): the playout worker at every look the timeline asks for before an arrival (none
/// when the packet says the worker was stalled until it), the packet, the worker at the arrival; a
/// PACKET is stored, an anchor clears the store. A parity driver, not a timing model: the shipped
/// worker's wake schedule is modelled in src/vban_receive_timeline_worker_model.rs.
#[derive(Default)]
struct Drive {
    store: VecDeque<u64>,
    clock: u64,
}

impl Drive {
    /// The worker at every deadline before `until` (at it too when `inclusive`); each decision is
    /// handed to `seen`.
    fn worker(
        &mut self,
        t: &mut Timeline,
        until: u64,
        inclusive: bool,
        seen: &mut dyn FnMut(&Timeline, DueDecision, Look),
    ) {
        loop {
            let look = Look {
                head: self.store.front().copied(),
                prev_audio: t.prev_audio,
                run: t.conceal_run,
            };
            let d = t.due(self.clock, look.head);
            seen(t, d, look);
            match d.kind {
                Due::None => {
                    let due =
                        d.next_ns != 0 && (d.next_ns < until || (inclusive && d.next_ns == until));
                    if !due {
                        break;
                    }
                    self.clock = self.clock.max(d.next_ns);
                }
                Due::Packet => {
                    self.store.pop_front();
                }
                _ => {}
            }
        }
        self.clock = self.clock.max(until);
    }

    /// One packet, the worker around it.
    fn packet(
        &mut self,
        t: &mut Timeline,
        p: &Pkt,
        seen: &mut dyn FnMut(&Timeline, DueDecision, Look),
    ) -> Decision {
        if p.reset {
            *t = Timeline::new();
            self.store.clear();
        }
        if p.stall {
            self.clock = self.clock.max(p.now);
        } else {
            self.worker(t, p.now, false, seen);
        }
        let d = t.packet(p.now, p.nu, p.frames, p.rate, p.channels, p.reserve_ns);
        match d.event {
            Event::Anchor | Event::Reanchor => self.store.clear(),
            Event::Packet | Event::Reorder => self.store_insert(d.pos),
            _ => {}
        }
        d
    }

    /// The store stays in slot order: a reordered packet goes in before the later slots
    /// (ROZHODNUTÉ 6352253147 (a)), exactly like the C driver's `store_insert`.
    fn store_insert(&mut self, pos: u64) {
        let at = self.store.partition_point(|&p| p < pos);
        self.store.insert(at, pos);
    }
}

fn bucket_dump(t: &Timeline) -> String {
    let mut s = String::from("buckets");
    for b in &t.buckets {
        write!(
            s,
            " {} {} {} {} {} {} {} {}",
            b.k,
            b.n,
            b.x0,
            b.y0,
            bits(b.su),
            bits(b.sv),
            bits(b.suu),
            bits(b.suv)
        )
        .unwrap();
    }
    s
}

fn rust_trace(scripts: &[Vec<Pkt>]) -> Vec<String> {
    let mut out = Vec::new();
    for &ms in &CLAMP_INPUTS {
        out.push(format!("clamp {}", clamp_reserve_ms(ms)));
    }
    for &(s, r) in &NS_INPUTS {
        out.push(format!("ns {}", samples_to_ns(s, r)));
    }
    for &(ns, r) in &SAMPLES_INPUTS {
        out.push(format!("samples {}", ns_to_samples(ns, r)));
    }
    for &(ns, ppb) in &SCALE_INPUTS {
        out.push(format!("scale {}", scale_ppb(ns, ppb)));
    }
    for &x in &ROUND_INPUTS {
        out.push(format!("round {}", round_half_away(x)));
    }
    for sc in scripts {
        let mut t = Timeline::new();
        let mut drive = Drive::default();
        for (i, p) in sc.iter().enumerate() {
            let mut dues = Vec::new();
            let d = drive.packet(&mut t, p, &mut |t, d, _| dues.push(due_line(t, d)));
            out.append(&mut dues);
            out.push(line(&t, d));
            drive.worker(&mut t, p.now, true, &mut |t, d, _| out.push(due_line(t, d)));
            if i % 97 == 96 {
                out.push(bucket_dump(&t));
            }
        }
    }
    out
}

fn c_driver(scripts: &[Vec<Pkt>]) -> String {
    let header = repo(HEADER);
    let mut c = String::new();
    writeln!(
        c,
        "#include <stdio.h>\n#include <inttypes.h>\n#include <string.h>"
    )
    .unwrap();
    writeln!(c, "#include \"{}\"", header.display()).unwrap();
    c.push_str(
        r#"
static uint64_t bits(double x)
{
	uint64_t u;
	memcpy(&u, &x, sizeof(u));
	return u;
}

static void line(const struct vban_recv_timeline *t, struct vban_recv_decision d)
{
	const struct vban_recv_bucket *b = &t->buckets[(size_t)(((t->last_k % VBAN_RECV_FIT_WINDOW_S) + VBAN_RECV_FIT_WINDOW_S) % VBAN_RECV_FIT_WINDOW_S)];
	printf("%d %d %" PRIu64 " %" PRIu64 " %" PRId64 " %" PRId64 " %" PRId64 " | ", d.event, d.reason, d.pos,
	       d.stamp_ns, d.lateness_ns, d.late_ns, d.starve_ns);
	printf("%d %" PRIu32 " %" PRIu32 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu32 " %" PRIu64 " %" PRIu64 " %" PRIu64
	       " %" PRId64 " %" PRId64 " %" PRIu64 " %d %" PRId64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRId64 " %" PRIu64
	       " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " | ",
	       t->anchored ? 1 : 0, t->rate, t->channels, t->reserve_ns, t->reserve_eff_ns, t->anchor_ns, t->anchor_end,
	       t->last_nu,
	       t->next_pos, t->base_stamp_ns, t->base_pos, t->step_ppb, t->last_k, t->last_intime_ns,
	       t->fit_valid ? 1 : 0, t->fit_ppb, bits(t->fit_b), bits(t->fit_xbar), bits(t->fit_ybar), t->phase_ns,
	       t->received, t->lost, t->silence_samples, t->late, t->re_anchors);
	printf("%" PRIu64 " %" PRIu64 " %d %" PRIu32 " %d %" PRIu64 " %d | ", t->stale, t->last_fold_end,
	       vban_recv_fit_frozen(t) ? 1 : 0, t->frames, t->emitted ? 1 : 0, t->emit_pos, t->lead_in_pending ? 1 : 0);
	printf("%" PRId64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " | %" PRIu64 " %" PRIu64,
	       b->k, b->n, b->x0, b->y0, bits(b->su), bits(b->sv), bits(b->suu), bits(b->suv),
	       vban_recv_stamp_at(t, t->next_pos + 1000),
	       vban_recv_stamp_at(t, t->base_pos > 1000 ? t->base_pos - 1000 : 0));
	printf(" | %" PRIu64 " %" PRIu64 "\n", t->recv_mask, t->reordered);
}

static void due_line(const struct vban_recv_timeline *t, struct vban_recv_due d)
{
	printf("due %d %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRId64 " %" PRIu64 " %d | %" PRIu64 " %d %" PRIu64 " %" PRIu64 " %" PRIu64 " | %d %" PRIu32 " %" PRIu64 "\n",
	       d.kind, d.pos, d.frames, d.stamp_ns, d.deadline_ns, d.late_ns, d.next_ns, d.toward_next ? 1 : 0,
	       t->emit_pos, t->lead_in_pending ? 1 : 0, t->lost, t->silence_samples, t->missed, t->prev_audio ? 1 : 0,
	       t->conceal_run, t->concealed);
}

/* the store of in-time packets and the worker's clock (the Rust Drive) */
#define STORE_CAP 65536u
static uint64_t store[STORE_CAP];
static size_t store_head, store_tail;
static uint64_t clk;

static void worker(struct vban_recv_timeline *t, uint64_t until, int inclusive)
{
	for (;;) {
		const bool has = store_head != store_tail;
		const struct vban_recv_due d = vban_recv_due(t, clk, has, has ? store[store_head % STORE_CAP] : 0);
		due_line(t, d);
		if (d.kind == VBAN_RECV_DUE_NONE) {
			const int due = d.next_ns != 0 && (d.next_ns < until || (inclusive && d.next_ns == until));
			if (!due)
				break;
			if (d.next_ns > clk)
				clk = d.next_ns;
		} else if (d.kind == VBAN_RECV_DUE_PACKET) {
			store_head++;
		}
	}
	if (until > clk)
		clk = until;
}

/* the store stays in slot order: a reordered packet goes in before the later slots (the Rust Drive) */
static void store_insert(uint64_t pos)
{
	size_t i = store_tail;
	while (i > store_head && store[(i - 1) % STORE_CAP] > pos) {
		store[i % STORE_CAP] = store[(i - 1) % STORE_CAP];
		i--;
	}
	store[i % STORE_CAP] = pos;
	store_tail++;
}

static void bucket_dump(const struct vban_recv_timeline *t)
{
	printf("buckets");
	for (size_t i = 0; i < VBAN_RECV_FIT_WINDOW_S; i++) {
		const struct vban_recv_bucket *b = &t->buckets[i];
		printf(" %" PRId64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64, b->k,
		       b->n, b->x0, b->y0, bits(b->su), bits(b->sv), bits(b->suu), bits(b->suv));
	}
	printf("\n");
}

struct pkt {
	uint64_t now;
	uint32_t nu, frames, rate, channels;
	uint64_t reserve_ns;
	int reset;
	int stall;
};
"#,
    );
    for (i, sc) in scripts.iter().enumerate() {
        writeln!(c, "static const struct pkt script_{i}[] = {{").unwrap();
        for p in sc {
            writeln!(
                c,
                "\t{{{}ULL, {}U, {}U, {}U, {}U, {}ULL, {}, {}}},",
                p.now,
                p.nu,
                p.frames,
                p.rate,
                p.channels,
                p.reserve_ns,
                i32::from(p.reset),
                i32::from(p.stall)
            )
            .unwrap();
        }
        writeln!(c, "}};").unwrap();
    }
    writeln!(c, "static struct vban_recv_timeline t;\nint main(void)\n{{").unwrap();
    for ms in CLAMP_INPUTS {
        writeln!(
            c,
            "\tprintf(\"clamp %\" PRIu32 \"\\n\", vban_recv_clamp_reserve_ms({ms}LL));"
        )
        .unwrap();
    }
    for (s, r) in NS_INPUTS {
        writeln!(
            c,
            "\tprintf(\"ns %\" PRIu64 \"\\n\", vban_recv_samples_to_ns({s}ULL, {r}U));"
        )
        .unwrap();
    }
    for (ns, r) in SAMPLES_INPUTS {
        writeln!(
            c,
            "\tprintf(\"samples %\" PRIu64 \"\\n\", vban_recv_ns_to_samples({ns}ULL, {r}U));"
        )
        .unwrap();
    }
    for (ns, ppb) in SCALE_INPUTS {
        writeln!(
            c,
            "\tprintf(\"scale %\" PRId64 \"\\n\", vban_recv_scale_ppb({ns}ULL, {ppb}LL));"
        )
        .unwrap();
    }
    for x in ROUND_INPUTS {
        writeln!(
            c,
            // read through a volatile, so the compiler cannot fold the conversion at build time
            "\t{{ static volatile uint64_t vu; vu = {}ULL; uint64_t u = vu; double x; memcpy(&x, &u, sizeof(x)); printf(\"round %\" PRId64 \"\\n\", vban_recv_round(x)); }}",
            x.to_bits()
        )
        .unwrap();
    }
    for i in 0..scripts.len() {
        writeln!(
            c,
            "\tvban_recv_init(&t);\n\
             \tstore_head = store_tail = 0;\n\
             \tclk = 0;\n\
             \tfor (size_t i = 0; i < sizeof(script_{i}) / sizeof(script_{i}[0]); i++) {{\n\
             \t\tconst struct pkt *p = &script_{i}[i];\n\
             \t\tif (p->reset) {{\n\
             \t\t\tvban_recv_init(&t);\n\
             \t\t\tstore_head = store_tail;\n\
             \t\t}}\n\
             \t\tif (p->stall) {{\n\
             \t\t\tif (p->now > clk)\n\
             \t\t\t\tclk = p->now;\n\
             \t\t}} else {{\n\
             \t\t\tworker(&t, p->now, 0);\n\
             \t\t}}\n\
             \t\tconst struct vban_recv_decision d = vban_recv_packet(&t, p->now, p->nu, p->frames, p->rate, p->channels, p->reserve_ns);\n\
             \t\tif (d.event == VBAN_RECV_ANCHOR || d.event == VBAN_RECV_REANCHOR)\n\
             \t\t\tstore_head = store_tail;\n\
             \t\telse if (d.event == VBAN_RECV_PACKET || d.event == VBAN_RECV_REORDER)\n\
             \t\t\tstore_insert(d.pos);\n\
             \t\tline(&t, d);\n\
             \t\tworker(&t, p->now, 1);\n\
             \t\tif (i % 97 == 96)\n\
             \t\t\tbucket_dump(&t);\n\
             \t}}"
        )
        .unwrap();
    }
    writeln!(c, "\treturn 0;\n}}").unwrap();
    c
}

#[test]
fn c_receive_timeline_matches_the_rust_authority_1439() {
    assert!(
        repo(HEADER).is_file(),
        "issue 1439: {HEADER} is missing -- the obs-vban source stamps at arrival again"
    );
    let scripts = scripts();
    let c = compile_and_run("parity", &c_driver(&scripts), &[]);
    let r = rust_trace(&scripts);
    assert_eq!(
        c.len(),
        r.len(),
        "issue 1439: C and Rust traces differ in length"
    );
    let diffs: Vec<String> = c
        .iter()
        .zip(&r)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(5)
        .map(|(i, (a, b))| format!("line {i}:\n  C    `{a}`\n  Rust `{b}`"))
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1439: the vendored receive timeline diverges from src/vban_receive_timeline.rs:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn the_scripts_reach_every_boundary_1439() {
    // A parity gate is only as good as the states it visits (the libobs tie-break lesson).
    let names = [
        "anchors",
        "slots out as silence (round 5: at their deadline)",
        "starve re-anchors on a late packet (a delay step or a pause)",
        "packets exactly on their deadline (in time)",
        "backward re-anchors",
        "jump re-anchors",
        "format re-anchors",
        "config re-anchors",
        "fit re-anchors: rate beyond the clamp",
        "fit re-anchors: phase beyond R",
        "late drops exactly one second after the last in-time arrival (still a drop)",
        "fits becoming valid",
        "stamp step changes",
        "outlier exclusions",
        "packets within 1 ns of the outlier line",
        "index wraps",
        "losses of exactly two seconds",
        "stale duplicates (dropped)",
        "stale reorders at the window's edge (dropped)",
        "backward re-anchors one past the stale window",
        "anchor moves to an earlier arrival in the first second",
        "earlier arrivals past the first second (the line kept)",
        "phase term acting outside the dead band",
        "rate-only step changes inside the dead band",
        "late drops after the first output (their slot already out)",
        "late drops with lost slots before them",
        "anchors with R lifted to one packet + 10 ms",
        "anchors with R above that floor",
        "held packets (an anchor's first second, never output)",
        "a loss inside the hold",
        "the first emitted packet after a hold",
        "starve re-anchors on an on-time packet (after an outage)",
        "starve re-anchors one ns past the second",
        "re-anchors during the hold",
        "a late first packet past the hold (dropped, nothing filled)",
        "lead-ins (the worker, once per stretch)",
        "a second late packet before a stretch's first output (dropped, nothing filled)",
        "frozen fits (valid, nothing folded for over FROZEN_S of audio)",
        "a fit exactly FROZEN_S of audio past its last fold (not frozen yet)",
        "a frozen fit thawing (a packet folds again)",
        "starve re-anchors during the hold",
        "a starve on the first packet after an anchor",
        "starve re-anchors carrying their span (since the last in-time arrival)",
        "a loss between a late packet and the stretch's first output (not filled)",
        "slots out as their packet",
        "a packet one ns past its deadline before the first output (late)",
        "a packet one ns past its deadline after the first output (late)",
        "a stretch starved (the worker idle while it emits)",
        "a slot read back before the line's base (rebased while it waited)",
        "another packet size (a format re-anchor)",
        "slots out as their packet ahead of their deadline (round 5: at the worker's look)",
        "a look again after the poll (the slot's packet not there yet)",
        "slots out after their stamp (missed: a stalled worker)",
        "a stretch with no room for a lead-in (its first packet at its deadline)",
        "reorders stored for their slot (ROZHODNUTÉ 6352253147 (a))",
        "a duplicate of a packet that came (stale)",
        "a reorder whose slot already went out (stale)",
        "a reorder in a starved stretch (stale)",
        "a reorder before the stretch's first output (stale)",
        "concealed slots fading toward the next packet (ROZHODNUTÉ 6352253147 (b))",
        "concealed slots with the next one missing too",
        "the third slot of a concealed run (the run limit)",
        "a run of four missing slots (silence)",
        "a missing slot with nothing stored at its deadline (silence)",
        "a run that started as silence with a packet stored behind it (stays silence)",
    ];
    let mut hits = [0usize; 65];
    // what the worker hands out (both twins' drivers do the same): every slot one slot, its silence
    // never before its deadline (a stored packet may go out ahead of it, review round 5)
    fn tally(hits: &mut [usize; 65], t: &Timeline, d: DueDecision, look: Look) {
        // how many slots on the next stored packet sits (0 = nothing stored)
        let next = look
            .head
            .map_or(0, |h| h.saturating_sub(d.pos) / u64::from(t.frames.max(1)));
        match d.kind {
            Due::Conceal => {
                assert_eq!(d.frames, u64::from(t.frames), "one slot: {d:?}");
                assert!(d.late_ns >= 0, "a concealment before its deadline: {d:?}");
                assert!(
                    look.prev_audio && next >= 1,
                    "concealed between packets: {d:?}"
                );
                hits[if d.toward_next { 59 } else { 60 }] += 1;
                hits[61] += usize::from(look.run == 2);
            }
            Due::Silence | Due::Packet => {
                assert_eq!(d.frames, u64::from(t.frames), "one slot: {d:?}");
                if d.kind == Due::Silence {
                    assert!(d.late_ns >= 0, "a silence before its deadline: {d:?}");
                }
                hits[if d.kind == Due::Silence { 1 } else { 44 }] += 1;
                if d.kind == Due::Silence {
                    hits[62] += usize::from(look.prev_audio && next == 4);
                    hits[63] += usize::from(look.prev_audio && look.head.is_none());
                    hits[64] += usize::from(!look.prev_audio && (1..=3).contains(&next));
                }
                hits[48] += usize::from(d.pos < t.base_pos);
                hits[50] += usize::from(d.kind == Due::Packet && d.late_ns < 0);
                hits[52] += usize::from(d.late_ns > (DEADLINE_MARGIN_MS * MS) as i64);
            }
            Due::LeadIn => hits[35] += 1,
            Due::None => {
                hits[47] += usize::from(d.next_ns == 0 && t.emitted);
                hits[51] += usize::from(
                    t.emitted && d.next_ns != 0 && d.next_ns < t.deadline_at(t.emit_pos),
                );
            }
        }
    }
    for sc in scripts() {
        let mut t = Timeline::new();
        let mut drive = Drive::default();
        // the last arrival that was in time (an anchor, a held or an emitted packet), harness-side
        let mut last_in = 0u64;
        // the previous decision's event
        let mut prev_event = Event::Stale;
        for p in &sc {
            if p.reset {
                t = Timeline::new();
                drive.store.clear();
            }
            if p.stall {
                drive.clock = drive.clock.max(p.now);
            } else {
                drive.worker(&mut t, p.now, false, &mut |t, d, l| {
                    tally(&mut hits, t, d, l)
                });
            }
            let frames_before = t.frames;
            let (anchored, valid, step, last_nu) = (t.anchored, t.fit_valid, t.step_ppb, t.last_nu);
            let emitted_before = t.emitted;
            // a packet up to 63 behind the last one (ROZHODNUTÉ 6352253147 (a)): did its index come,
            // did its slot go out, did the stretch starve
            let back_by = t.last_nu.wrapping_sub(p.nu);
            let behind = anchored
                && p.rate == t.rate
                && p.channels == t.channels
                && p.frames == t.frames
                && p.reserve_ns == t.reserve_ns
                && (1..64).contains(&back_by);
            let came = behind && (t.recv_mask >> back_by) & 1 == 1;
            let slot_out = behind
                && t.next_pos
                    .checked_sub(u64::from(back_by + 1) * u64::from(p.frames))
                    .is_none_or(|pos| pos < t.emit_pos);
            let starved = p.now as i64 - t.last_intime_ns as i64 > 1_000_000_000;
            let frozen_before = t.fit_frozen();
            let (base_before, base_pos_before, next_before) =
                (t.base_stamp_ns, t.base_pos, t.next_pos);
            let since_in = p.now as i64 - last_in as i64;
            let folds = t.buckets.iter().map(|b| b.n).sum::<u64>();
            // which fit check would fail: the model's update run on a copy, exactly when the packet
            // takes the timeline into a new bucket (the only time the real decision refits)
            let delta = p.nu.wrapping_sub(t.last_nu);
            let same = anchored
                && p.rate == t.rate
                && p.channels == t.channels
                && p.reserve_ns == t.reserve_ns
                && delta != 0
                && delta < 0x8000_0000;
            let (mut stamp, mut clamp_fail, mut phase_fail, mut line_y) = (0, false, false, 0.0);
            let mut deadline = 0u64;
            // the anchor this packet would give, carried back to position 0 (the first-second rule)
            let (mut end, mut cand) = (0u64, 0u64);
            if same {
                let lost = u64::from(delta - 1);
                let pos = t.next_pos + lost * u64::from(p.frames);
                end = pos + u64::from(p.frames);
                cand = p.now.saturating_sub(
                    samples_to_ns(u64::from(p.frames), p.rate) + samples_to_ns(pos, p.rate),
                ) + t.reserve_eff_ns;
                if (end / u64::from(p.rate)) as i64 > t.last_k {
                    let mut probe = t.clone();
                    if !probe.update(t.next_pos) {
                        let refit = probe.fit_valid != t.fit_valid
                            || probe.fit_b.to_bits() != t.fit_b.to_bits();
                        phase_fail = refit;
                        clamp_fail = !refit;
                    }
                }
                stamp = t.stamp_at(pos);
                deadline = t.deadline_at(pos);
                line_y = t.reference(end) + (OUTLIER_MS * MS) as f64;
            }
            let d = t.packet(p.now, p.nu, p.frames, p.rate, p.channels, p.reserve_ns);
            match d.event {
                Event::Anchor | Event::Reanchor => drive.store.clear(),
                Event::Packet | Event::Reorder => drive.store_insert(d.pos),
                _ => {}
            }
            let back = last_nu.wrapping_sub(p.nu);
            match (d.event, d.reason) {
                (Event::Stale, _) if back == 0 => hits[17] += 1,
                (Event::Stale, _) if back == STALE_PACKETS => hits[18] += 1,
                (Event::Reanchor, Reason::Backward) if back == STALE_PACKETS + 1 => {
                    hits[4] += 1;
                    hits[19] += 1;
                }
                (Event::Anchor, _) => hits[0] += 1,
                (Event::Late, _) => {
                    hits[24] += usize::from(emitted_before);
                    hits[25] += usize::from(same && delta > 1);
                    hits[10] += usize::from(since_in == 1_000_000_000);
                    hits[34] +=
                        usize::from(same && !emitted_before && next_before <= u64::from(p.rate));
                    hits[45] += usize::from(same && !emitted_before && p.now == deadline + 1);
                    hits[46] += usize::from(same && emitted_before && p.now == deadline + 1);
                }
                (Event::Reanchor, Reason::Starve) => {
                    hits[2] += usize::from(same && p.now > stamp);
                    hits[31] += usize::from(same && p.now <= stamp);
                    hits[32] += usize::from(since_in == 1_000_000_001);
                }
                (Event::Reanchor, Reason::Backward) => hits[4] += 1,
                (Event::Reanchor, Reason::Jump) => hits[5] += 1,
                (Event::Reanchor, Reason::Format) => hits[6] += 1,
                (Event::Reanchor, Reason::Config) => hits[7] += 1,
                (Event::Reanchor, Reason::Fit) => {
                    if clamp_fail {
                        hits[8] += 1;
                    } else {
                        assert!(
                            phase_fail,
                            "a fit re-anchor is the rate clamp or the R hold: {d:?}"
                        );
                        hits[9] += 1;
                    }
                }
                _ => {}
            }
            hits[54] += usize::from(d.event == Event::Reorder);
            if d.event == Event::Stale && behind {
                hits[55] += usize::from(emitted_before && came);
                hits[56] += usize::from(emitted_before && !came && !starved && slot_out);
                hits[57] += usize::from(emitted_before && !came && starved);
                hits[58] += usize::from(!emitted_before);
            }
            hits[16] += usize::from(
                same && d.event == Event::Packet
                    && u64::from(delta - 1) * u64::from(p.frames) == JUMP_S * u64::from(p.rate),
            );
            if d.event == Event::Packet && same && p.now == deadline {
                hits[3] += 1;
            }
            hits[11] += usize::from(!valid && t.fit_valid);
            hits[12] += usize::from(d.event == Event::Packet && t.step_ppb != step);
            let folded = t.buckets.iter().map(|b| b.n).sum::<u64>();
            if d.event == Event::Packet && same {
                let y = p.now.wrapping_sub(t.anchor_ns) as f64;
                hits[13] += usize::from(folded == folds && y > line_y);
                hits[14] += usize::from((y - line_y).abs() <= 1.0);
            }
            hits[15] += usize::from(d.event == Event::Packet && same && p.nu < last_nu);
            let initial_line = base_pos_before == 0 && step == 0;
            let in_first_second = end <= u64::from(p.rate);
            if d.event == Event::Hold && same && initial_line {
                hits[20] += usize::from(in_first_second && t.base_stamp_ns < base_before);
            }
            if d.event == Event::Packet && same && initial_line {
                hits[21] += usize::from(!in_first_second && cand < base_before);
            }
            hits[28] += usize::from(d.event == Event::Hold);
            hits[29] += usize::from(d.event == Event::Hold && same && delta > 1);
            hits[30] += usize::from(
                d.event == Event::Packet
                    && same
                    && next_before <= u64::from(p.rate)
                    && end > u64::from(p.rate),
            );
            hits[33] += usize::from(
                d.event == Event::Reanchor && anchored && next_before <= u64::from(p.rate),
            );
            let first_output = !emitted_before && d.event == Event::Packet;
            hits[53] += usize::from(
                first_output
                    && d.stamp_ns < p.now + DEADLINE_MARGIN_MS * MS + samples_to_ns(1, p.rate),
            );
            hits[36] += usize::from(
                d.event == Event::Late && !emitted_before && next_before > u64::from(p.rate),
            );
            hits[49] += usize::from(
                (d.event, d.reason) == (Event::Reanchor, Reason::Format)
                    && anchored
                    && p.rate == t.rate
                    && p.frames != frames_before,
            );
            hits[37] += usize::from(t.fit_frozen());
            hits[38] += usize::from(
                t.fit_valid
                    && t.next_pos.saturating_sub(t.last_fold_end) == FROZEN_S * u64::from(t.rate)
                    && !t.fit_frozen(),
            );
            hits[39] += usize::from(frozen_before && t.fit_valid && !t.fit_frozen());
            if (d.event, d.reason) == (Event::Reanchor, Reason::Starve) {
                hits[40] += usize::from(anchored && next_before <= u64::from(p.rate));
                hits[41] += usize::from(matches!(prev_event, Event::Anchor | Event::Reanchor));
                hits[42] += usize::from(d.starve_ns == since_in);
            }
            hits[43] += usize::from(first_output && same && delta > 1 && prev_event == Event::Late);
            prev_event = d.event;
            if matches!(
                d.event,
                Event::Anchor | Event::Reanchor | Event::Hold | Event::Packet
            ) {
                last_in = p.now;
            }
            if d.event == Event::Anchor || d.event == Event::Reanchor {
                hits[26] += usize::from(t.reserve_eff_ns != t.reserve_ns);
                hits[27] += usize::from(t.reserve_eff_ns == t.reserve_ns);
            }
            if d.event == Event::Packet && t.step_ppb != step {
                let band = (t.reserve_eff_ns / 4) as i64;
                hits[22] += usize::from(t.phase_ns.abs() > band);
                hits[23] += usize::from(t.phase_ns.abs() <= band);
            }
            drive.worker(&mut t, p.now, true, &mut |t, d, l| {
                tally(&mut hits, t, d, l)
            });
        }
    }
    for (name, n) in names.iter().zip(hits) {
        assert!(
            n >= 1,
            "issue 1439: the parity scripts hit no {name}: {hits:?}"
        );
    }
}
