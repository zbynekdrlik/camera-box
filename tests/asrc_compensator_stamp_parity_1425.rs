//! Issue 1425 (E2 slice 1, design 6206822792) — an EXECUTABLE C-vs-Rust parity gate for the STAMP
//! basis of the per-source ASRC: its CONFIRMED placement-jump booking.
//!
//! `src/asrc_bench_timecode.rs` (`set_basis`, `observe_placement` and the confirmation candidate) is
//! the Tier-0 authority; `vendor/obs-studio/libobs/media-io/asrc-compensator.c` is its port. This
//! gate compiles the REAL `asrc-compensator.c` (its own header, no stub) under `-Wall -Wextra
//! -Wconversion -Wformat=2 -Werror`, drives it from a small C `main` and requires the printed
//! 9-decimal trace to be byte-identical to the Rust authority driven statement for statement:
//!
//! - `st`: a closed-loop placement-error plant on the stamp basis, 128-sample packets with a
//!   ±1.5 ms jitter pattern: a 30 ms and a 120 ms delivery-stall transient (dropped, never booked), a
//!   real 44 ms loss and a 44 ms duplicate (booked once each after one second of audio, paid at
//!   1000 ppm), an excursion back inside the band before the second (dropped), a sign flip (a new
//!   candidate), a placement inside an open candidate, a stamp → timecode → stamp change inside one
//!   (the timecode basis books at once) and a leave of the mode (a flush, inert).
//! - `str`: open loop on a locked servo: 16-sample packets (the ring wraps; the median of the newest
//!   256), 1500-sample packets (an even last half: the two middle errors averaged) and 200 ms packets
//!   (the 8-packet gate).
//! - `stx` (mutation round): the reference stays the one at the open across a window close, a lost
//!   and re-taken capture and a placement each start a new candidate, a median inside the band
//!   against the setpoint at the confirmation books nothing, and a jump the owed cap refuses leaves
//!   the smoothed error alone.
//!
//! The issue-1367 scenarios (`tests/asrc_compensator_parity_1367.rs`) keep the timecode and arrival
//! bases; this file adds only the stamp basis, so neither file grows past its budget. It also pins
//! the basis values shared with obs-source.c and the confirmation constants. Per the project's
//! test-strictness rule it FAILS LOUDLY rather than skipping when the C toolchain is missing.

use camera_box::asrc_bench::{
    RealtimeAsrcCompensator, PLACE_CONFIRM_MS, PLACE_CONFIRM_PACKETS, PLACE_CONFIRM_RING,
};
use camera_box::genlock_audio_pairing::AsrcBasis;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const MEDIA_IO: &str = "vendor/obs-studio/libobs/media-io";
const C_SRC: &str = "vendor/obs-studio/libobs/media-io/asrc-compensator.c";
const C_HDR: &str = "vendor/obs-studio/libobs/media-io/asrc-compensator.h";
const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The C driver. It mirrors `rust_trace()` below statement for statement.
const HARNESS_C: &str = r##"#include <stdint.h>
#include <stdio.h>
#include <stdbool.h>
#include "asrc-compensator.h"

static void stline(const char *tag, long k, const struct asrc_compensator *c, double recp)
{
	printf("%s k=%ld est=%.9f app=%.9f lvl=%.9f avg=%.9f tgt=%.9f int=%.9f ema=%.9f rst=%d steps=%u rec=%.9f recp=%.9f tc=%d basis=%d jumps=%u lastj=%.9f cand=%u\n",
	       tag, k, c->estimated_ppm, c->applied_ppm, c->level_last_ms, c->level_avg_ms, c->level_target_ms,
	       c->level_integral_ppm, c->level_err_ema_ms, c->level_restore ? 1 : 0, c->step_count, c->step_recover_ms,
	       recp, c->timecode ? 1 : 0, c->basis, c->place_jump_count, c->last_place_jump_ms,
	       c->place_cand_open ? c->place_cand_packets : 0u);
}

static bool near(long k, long e)
{
	return (k >= e - 1 && k <= e + 2) || (k >= e + 373 && k <= e + 379);
}

/* the stamp basis, closed loop: the plant is the packet's placement error against its stamp (the
 * stamps advance 4 ppm faster than the samples, each packet is resampled by -(applied + the taken
 * recovery)), plus a +/-1.5 ms jitter pattern, a delivery-stall burst ramp and an excursion. */
static void st_scenario(void)
{
	const double packet_s = 128.0 / 48000.0;
	const double packet_ms = packet_s * 1000.0;
	const double pattern[4] = {-1.5, 0.5, 1.0, -0.5};
	struct asrc_compensator c;
	asrc_compensator_init(&c);
	asrc_compensator_set_level_absolute(&c, false);
	asrc_compensator_set_basis(&c, ASRC_BASIS_STAMP);
	double place_ms = 0.0;
	double burst_ms = 0.0;
	double applied = 0.0;
	double recp = 0.0;
	for (long k = 0; k < 120000; k++) {
		bool placed = false;
		double master_s = packet_s * (1.0 + 4.0 / 1000000.0);
		double hold_ms = 0.0;
		if (k == 30000)
			burst_ms = -30.0;
		if (k == 40000)
			burst_ms = -120.0;
		if (k == 50000)
			place_ms -= 44.0;
		if (k == 70000)
			place_ms += 44.0;
		if (k >= 80000 && k < 80200)
			hold_ms = -44.0;
		if (k == 90000)
			place_ms -= 44.0;
		if (k == 90150)
			place_ms += 88.0;
		if (k == 99900)
			place_ms -= 44.0;
		if (k == 100000) {
			place_ms = 0.0;
			placed = true;
		}
		if (k == 105000)
			place_ms -= 44.0;
		if (k == 105100)
			asrc_compensator_set_basis(&c, ASRC_BASIS_TIMECODE);
		if (k == 105101)
			asrc_compensator_set_basis(&c, ASRC_BASIS_STAMP);
		if (k == 110000)
			asrc_compensator_set_basis(&c, ASRC_BASIS_ARRIVAL);
		if (k == 110001)
			asrc_compensator_set_basis(&c, ASRC_BASIS_STAMP);
		const double err = place_ms + burst_ms + hold_ms + pattern[k % 4];
		if (burst_ms < 0.0) {
			burst_ms += packet_ms;
			if (burst_ms > 0.0)
				burst_ms = 0.0;
		}
		asrc_compensator_observe_placement(&c, err, packet_ms, placed);
		if (!placed && master_s > 0.0)
			asrc_compensator_compensate(&c, packet_s, master_s, err, &applied);
		recp = asrc_compensator_take_step_recover_ppm(&c);
		place_ms += -(applied + recp) / 1000000.0 * packet_ms - 4.0 / 1000000.0 * packet_ms;
		if (k % 1875 == 1874 || near(k, 30000) || near(k, 40000) || near(k, 50000) || near(k, 70000) ||
		    near(k, 80000) || near(k, 80200) || near(k, 90000) || near(k, 90150) || near(k, 99900) ||
		    near(k, 100000) || near(k, 105000) || near(k, 105100) || near(k, 110000))
			stline("st", k, &c, recp);
	}
}

/* open loop on a locked, captured stamp servo fed `packet_s` packets at a steady 0 for 120 s */
static void st_lock(struct asrc_compensator *c, double packet_s)
{
	double applied = 0.0;
	asrc_compensator_init(c);
	asrc_compensator_set_basis(c, ASRC_BASIS_STAMP);
	for (long k = 0; k < (long)(120.0 / packet_s); k++) {
		asrc_compensator_observe_placement(c, 0.0, packet_s * 1000.0, false);
		asrc_compensator_compensate(c, packet_s, packet_s, 0.0, &applied);
		(void)asrc_compensator_take_step_recover_ppm(c);
	}
}

static void str_scenario(void)
{
	struct asrc_compensator c;
	/* 16-sample packets: the ring wraps, the newest 256 give the median */
	st_lock(&c, 0.002);
	for (long k = 0; k < 3003; k++) {
		const double err = k < 2903 ? -30.0 : -50.0 - (double)(k % 7);
		asrc_compensator_observe_placement(&c, err, 16.0 / 48.0, false);
		if (k % 500 == 499 || k >= 2998)
			stline("str16", k, &c, 0.0);
	}
	/* 1500-sample packets: an even last half, its two middle errors averaged */
	st_lock(&c, 1500.0 / 48000.0);
	for (long k = 0; k < 33; k++) {
		const double err = k < 24 ? -40.0 - (double)(k % 3) : -50.0 - (double)(k % 2);
		asrc_compensator_observe_placement(&c, err, 1500.0 / 48.0, false);
		stline("str1500", k, &c, 0.0);
	}
	/* 200 ms packets: a second after 5, the window needs 8 */
	st_lock(&c, 0.2);
	for (long k = 0; k < 9; k++) {
		asrc_compensator_observe_placement(&c, -150.0 + (double)k, 200.0, false);
		stline("str200", k, &c, 0.0);
	}
}

/* the candidate's edges, open loop on a locked 128-sample servo:
 * - stxema: a steady -10.5 ms error the level loop reads too, so a window close moves the smoothed
 *   error inside the candidate; judged against the reference at the open it still confirms;
 * - stxinert: a candidate, then the capture dropped (a capture-rule change) and re-taken while the
 *   placement error stays beyond the band: the re-taken capture starts a new candidate;
 * - stxplace: a placement inside a candidate starts a new one;
 * - stxband: a payment walks the setpoint up while the error tracks it 10.2 ms above it; the median of
 *   the last half lands inside the band against the setpoint at the confirmation: nothing is booked;
 * - stxcap: with the payment held, a second jump the owed cap refuses books nothing and leaves the
 *   smoothed error where the window took it. */
static void stx_scenario(void)
{
	const double p = 128.0 / 48000.0;
	const double pm = p * 1000.0;
	struct asrc_compensator c;
	double applied = 0.0;
	st_lock(&c, p);
	for (long k = 0; k < 800; k++) {
		asrc_compensator_observe_placement(&c, -10.5, pm, false);
		asrc_compensator_compensate(&c, p, p, -10.5, &applied);
		(void)asrc_compensator_take_step_recover_ppm(&c);
		if (k % 50 == 49 || (k >= 370 && k <= 380))
			stline("stxema", k, &c, 0.0);
	}
	st_lock(&c, p);
	for (long k = 0; k < 150; k++)
		asrc_compensator_observe_placement(&c, -44.0, pm, false);
	asrc_compensator_set_level_absolute(&c, false);
	for (long k = 0; k < 900; k++) {
		asrc_compensator_observe_placement(&c, -44.0, pm, false);
		asrc_compensator_compensate(&c, p, p, 0.0, &applied);
		(void)asrc_compensator_take_step_recover_ppm(&c);
		if (k % 25 == 24)
			stline("stxinert", k, &c, 0.0);
	}
	st_lock(&c, p);
	for (long k = 0; k < 500; k++) {
		asrc_compensator_observe_placement(&c, -44.0, pm, k == 100);
		if (k % 25 == 24 || k == 100 || k == 101)
			stline("stxplace", k, &c, 0.0);
	}
	st_lock(&c, p);
	for (long k = 0; k < 376; k++)
		asrc_compensator_observe_placement(&c, -50.0, pm, false);
	const double e0 = c.level_err_ema_ms;
	for (long k = 0; k < 800; k++) {
		const double err = c.level_target_ms + e0 + 10.2;
		asrc_compensator_observe_placement(&c, err, pm, false);
		asrc_compensator_compensate(&c, p, p, err, &applied);
		(void)asrc_compensator_take_step_recover_ppm(&c);
		if (k % 50 == 49 || (k >= 370 && k <= 380))
			stline("stxband", k, &c, 0.0);
	}
	st_lock(&c, p);
	asrc_compensator_set_step_recover_hold(&c, true);
	for (long k = 0; k < 800; k++) {
		asrc_compensator_observe_placement(&c, -150.0, pm, false);
		asrc_compensator_compensate(&c, p, p, -150.0, &applied);
		(void)asrc_compensator_take_step_recover_ppm(&c);
		if (k % 50 == 49 || (k >= 370 && k <= 380) || (k >= 745 && k <= 760))
			stline("stxcap", k, &c, 0.0);
	}
}

int main(void)
{
	st_scenario();
	str_scenario();
	stx_scenario();
	return 0;
}
"##;

fn stline(tag: &str, k: i64, c: &RealtimeAsrcCompensator, recp: f64) -> String {
    format!(
        "{tag} k={k} est={:.9} app={:.9} lvl={:.9} avg={:.9} tgt={:.9} int={:.9} ema={:.9} rst={} steps={} rec={:.9} recp={:.9} tc={} basis={} jumps={} lastj={:.9} cand={}",
        c.estimated_ppm(),
        c.applied_ppm(),
        c.level_last_ms(),
        c.level_avg_ms(),
        c.level_target_ms(),
        c.level_integral_ppm(),
        c.level_err_ema_ms(),
        u8::from(c.level_restore()),
        c.step_count(),
        c.step_recover_ms(),
        recp,
        u8::from(c.timecode()),
        c.basis().code(),
        c.place_jump_count(),
        c.last_place_jump_ms(),
        c.place_candidate_packets()
    )
}

fn near(k: i64, e: i64) -> bool {
    (e - 1..=e + 2).contains(&k) || (e + 373..=e + 379).contains(&k)
}

fn st_lock(packet_s: f64) -> RealtimeAsrcCompensator {
    let mut c = RealtimeAsrcCompensator::new();
    c.set_basis(AsrcBasis::Stamp);
    for _ in 0..(120.0 / packet_s) as i64 {
        c.observe_placement(0.0, packet_s * 1000.0, false);
        c.compensate_with_level(packet_s, packet_s, 0.0);
        c.take_step_recover_ppm();
    }
    c
}

/// The Rust authority driven exactly like `HARNESS_C` (statement for statement).
fn rust_trace() -> Vec<String> {
    let mut out = Vec::new();
    // st
    {
        let packet_s = 128.0 / 48000.0;
        let packet_ms = packet_s * 1000.0;
        let pattern = [-1.5, 0.5, 1.0, -0.5];
        let mut c = RealtimeAsrcCompensator::new();
        c.set_level_absolute(false);
        c.set_basis(AsrcBasis::Stamp);
        let mut place_ms = 0.0_f64;
        let mut burst_ms = 0.0_f64;
        let mut applied = 0.0_f64;
        for k in 0..120000_i64 {
            let mut placed = false;
            let master_s = packet_s * (1.0 + 4.0 / 1000000.0);
            let mut hold_ms = 0.0;
            if k == 30000 {
                burst_ms = -30.0;
            }
            if k == 40000 {
                burst_ms = -120.0;
            }
            if k == 50000 {
                place_ms -= 44.0;
            }
            if k == 70000 {
                place_ms += 44.0;
            }
            if (80000..80200).contains(&k) {
                hold_ms = -44.0;
            }
            if k == 90000 {
                place_ms -= 44.0;
            }
            if k == 90150 {
                place_ms += 88.0;
            }
            if k == 99900 {
                place_ms -= 44.0;
            }
            if k == 100000 {
                place_ms = 0.0;
                placed = true;
            }
            if k == 105000 {
                place_ms -= 44.0;
            }
            if k == 105100 {
                c.set_basis(AsrcBasis::Timecode);
            }
            if k == 105101 {
                c.set_basis(AsrcBasis::Stamp);
            }
            if k == 110000 {
                c.set_basis(AsrcBasis::Arrival);
            }
            if k == 110001 {
                c.set_basis(AsrcBasis::Stamp);
            }
            let err = place_ms + burst_ms + hold_ms + pattern[(k % 4) as usize];
            if burst_ms < 0.0 {
                burst_ms += packet_ms;
                if burst_ms > 0.0 {
                    burst_ms = 0.0;
                }
            }
            c.observe_placement(err, packet_ms, placed);
            if !placed && master_s > 0.0 {
                c.compensate_with_level(packet_s, master_s, err);
                applied = c.applied_ppm();
            }
            let recp = c.take_step_recover_ppm();
            place_ms += -(applied + recp) / 1000000.0 * packet_ms - 4.0 / 1000000.0 * packet_ms;
            if k % 1875 == 1874
                || [
                    30000, 40000, 50000, 70000, 80000, 80200, 90000, 90150, 99900, 100000, 105000,
                    105100, 110000,
                ]
                .iter()
                .any(|&e| near(k, e))
            {
                out.push(stline("st", k, &c, recp));
            }
        }
    }
    // str
    {
        let mut c = st_lock(0.002);
        for k in 0..3003_i64 {
            let err = if k < 2903 {
                -30.0
            } else {
                -50.0 - (k % 7) as f64
            };
            c.observe_placement(err, 16.0 / 48.0, false);
            if k % 500 == 499 || k >= 2998 {
                out.push(stline("str16", k, &c, 0.0));
            }
        }
        let mut c = st_lock(1500.0 / 48000.0);
        for k in 0..33_i64 {
            let err = if k < 24 {
                -40.0 - (k % 3) as f64
            } else {
                -50.0 - (k % 2) as f64
            };
            c.observe_placement(err, 1500.0 / 48.0, false);
            out.push(stline("str1500", k, &c, 0.0));
        }
        let mut c = st_lock(0.2);
        for k in 0..9_i64 {
            c.observe_placement(-150.0 + k as f64, 200.0, false);
            out.push(stline("str200", k, &c, 0.0));
        }
    }
    // stx
    {
        let p = 128.0 / 48000.0;
        let pm = p * 1000.0;
        let feed = |c: &mut RealtimeAsrcCompensator, err: f64, level: f64| {
            c.observe_placement(err, pm, false);
            c.compensate_with_level(p, p, level);
            c.take_step_recover_ppm();
        };
        let mut c = st_lock(p);
        for k in 0..800_i64 {
            feed(&mut c, -10.5, -10.5);
            if k % 50 == 49 || (370..=380).contains(&k) {
                out.push(stline("stxema", k, &c, 0.0));
            }
        }
        let mut c = st_lock(p);
        for _ in 0..150 {
            c.observe_placement(-44.0, pm, false);
        }
        c.set_level_absolute(false);
        for k in 0..900_i64 {
            feed(&mut c, -44.0, 0.0);
            if k % 25 == 24 {
                out.push(stline("stxinert", k, &c, 0.0));
            }
        }
        let mut c = st_lock(p);
        for k in 0..500_i64 {
            c.observe_placement(-44.0, pm, k == 100);
            if k % 25 == 24 || k == 100 || k == 101 {
                out.push(stline("stxplace", k, &c, 0.0));
            }
        }
        let mut c = st_lock(p);
        for _ in 0..376 {
            c.observe_placement(-50.0, pm, false);
        }
        let e0 = c.level_err_ema_ms();
        for k in 0..800_i64 {
            let err = c.level_target_ms() + e0 + 10.2;
            feed(&mut c, err, err);
            if k % 50 == 49 || (370..=380).contains(&k) {
                out.push(stline("stxband", k, &c, 0.0));
            }
        }
        let mut c = st_lock(p);
        c.set_step_recover_hold(true);
        for k in 0..800_i64 {
            feed(&mut c, -150.0, -150.0);
            if k % 50 == 49 || (370..=380).contains(&k) || (745..=760).contains(&k) {
                out.push(stline("stxcap", k, &c, 0.0));
            }
        }
    }
    out
}

fn c_trace() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("asrc_compensator_stamp_parity_1425");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let harness = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&harness, HARNESS_C).expect("write the parity harness");
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
            "-ffp-contract=off",
        ])
        .arg("-I")
        .arg(repo(MEDIA_IO))
        .arg(repo(C_SRC))
        .arg(&harness)
        .arg("-lm")
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1425: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored asrc-compensator.c to prove the C and the Rust authority agree; it must \
                 FAIL rather than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1425: {C_SRC} (+ the parity driver) does NOT COMPILE standalone under -Wall -Wextra \
         -Wconversion -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1425: the compiled parity harness failed to execute");
    assert!(
        run.status.success(),
        "issue 1425: the parity harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

fn field(line: &str, key: &str) -> f64 {
    let at = line
        .find(&format!(" {key}="))
        .unwrap_or_else(|| panic!("no {key}= in `{line}`"));
    let rest = &line[at + key.len() + 2..];
    rest[..rest.find(' ').unwrap_or(rest.len())]
        .parse()
        .unwrap_or_else(|e| panic!("{key}= in `{line}`: {e}"))
}

fn at<'a>(trace: &'a [String], tag: &str, k: i64) -> &'a String {
    let prefix = format!("{tag} k={k} ");
    trace
        .iter()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("issue 1425: no trace line `{prefix}`"))
}

#[test]
fn c_stamp_basis_matches_the_rust_authority_1425() {
    let c = c_trace();
    let r = rust_trace();
    assert_eq!(
        c.len(),
        r.len(),
        "issue 1425: the C and Rust drivers printed a different number of trace lines (C {} vs \
         Rust {})",
        c.len(),
        r.len()
    );
    for (i, (cl, rl)) in c.iter().zip(&r).enumerate() {
        assert_eq!(
            cl, rl,
            "issue 1425: asrc-compensator.c diverges from src/asrc_bench_timecode.rs at trace line \
             {i}:\n  C:    {cl}\n  Rust: {rl}"
        );
    }
    // the scenarios must reach the paths they claim: nothing booked by the two stalls or the short
    // excursion, one booking each for the loss, the duplicate, the flip, the placement and the
    // timecode change, the ring and the packet gate
    let jumps = |k: i64| field(at(&r, "st", k), "jumps");
    assert_eq!(jumps(30000 + 379), 0.0, "the 30 ms stall books nothing");
    assert_eq!(jumps(40000 + 379), 0.0, "the 120 ms stall books nothing");
    assert!(
        jumps(50000 + 373) == 0.0 && jumps(50000 + 379) == 1.0,
        "the 44 ms loss is booked once, one second later"
    );
    assert!(
        (field(at(&r, "st", 50000 + 379), "lastj") + 44.0).abs() < 2.0,
        "the loss is booked at its size"
    );
    assert_eq!(jumps(70000 + 379), 2.0, "the duplicate is booked once");
    assert_eq!(jumps(80200 + 379), 2.0, "the short excursion is dropped");
    assert!(
        jumps(90150 + 2) == 2.0 && jumps(90150 + 379) == 3.0,
        "the sign flip restarts the window and books the late side once"
    );
    assert_eq!(
        jumps(100000 + 379),
        3.0,
        "a placement drops the open candidate"
    );
    assert!(
        jumps(105100 + 1) == 4.0 && jumps(105100 + 379) == 4.0,
        "the timecode basis books the open excursion at once, and the stamp basis after it \
         never books it again"
    );
    assert!(
        field(at(&r, "st", 110000 + 2), "tc") == 1.0
            && field(at(&r, "st", 110000 + 2), "rec") == 0.0,
        "leaving the mode flushes"
    );
    assert!(
        field(at(&r, "str16", 2999), "jumps") == 1.0
            && (field(at(&r, "str16", 2999), "lastj") + 30.0).abs() < 1e-9,
        "the ring keeps the newest 256 errors"
    );
    let even = r
        .iter()
        .filter(|l| l.starts_with("str1500 "))
        .find(|l| field(l, "jumps") == 1.0)
        .expect("the 1500-sample window books");
    assert!(
        (field(even, "lastj") + 46.0).abs() < 1e-9,
        "an even last half averages its two middle errors: {even}"
    );
    assert!(
        field(at(&r, "str200", 6), "jumps") == 0.0 && field(at(&r, "str200", 7), "jumps") == 1.0,
        "the 8-packet gate holds a large-packet source"
    );
    let last = |tag: &str| {
        r.iter()
            .rev()
            .find(|l| l.starts_with(&format!("{tag} ")))
            .unwrap_or_else(|| panic!("no {tag} line"))
    };
    assert!(
        field(at(&r, "stxema", 380), "jumps") == 1.0
            && (field(at(&r, "stxema", 380), "lastj") + 10.5).abs() < 1e-9,
        "a window close inside the candidate does not move its reference"
    );
    assert_eq!(
        field(last("stxinert"), "jumps"),
        1.0,
        "the re-taken capture confirms its own candidate"
    );
    assert!(
        field(at(&r, "stxplace", 474), "jumps") == 0.0
            && field(at(&r, "stxplace", 499), "jumps") == 1.0,
        "a placement restarts the window"
    );
    assert_eq!(
        field(last("stxband"), "jumps"),
        1.0,
        "a median inside the band against the setpoint at the confirmation books nothing"
    );
    assert!(
        field(last("stxcap"), "jumps") == 1.0 && field(last("stxcap"), "rec") == 100.0,
        "the owed cap refuses the second jump"
    );
}

#[test]
fn the_basis_values_and_the_confirmation_constants_agree_1425() {
    let squish = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let h = squish(fs::read_to_string(repo(C_HDR)).expect("read the header"));
    let src = squish(fs::read_to_string(repo(OBS_SOURCE)).expect("read obs-source.c"));
    for (basis, name) in [
        (AsrcBasis::Arrival, "ARRIVAL"),
        (AsrcBasis::Timecode, "TIMECODE"),
        (AsrcBasis::Stamp, "STAMP"),
    ] {
        let code = basis.code();
        assert!(
            h.contains(&format!("#define ASRC_BASIS_{name} {code}"))
                && src.contains(&format!("#define GENLOCK_AUDIO_ASRC_BASIS_{name} {code}")),
            "issue 1425: the basis {name} must be {code} in asrc-compensator.h, obs-source.c and \
             AsrcBasis (obs-source.c passes its basis to asrc_compensator_set_basis as is)"
        );
    }
    assert!(
        h.contains(&format!(
            "#define ASRC_PLACE_CONFIRM_MS {PLACE_CONFIRM_MS:.1}"
        )) && h.contains(&format!(
            "#define ASRC_PLACE_CONFIRM_PACKETS {PLACE_CONFIRM_PACKETS}u"
        )) && h.contains(&format!(
            "#define ASRC_PLACE_CONFIRM_RING {PLACE_CONFIRM_RING}u"
        )),
        "issue 1425: the confirmation constants differ between asrc-compensator.h and \
         src/asrc_bench_timecode.rs"
    );
}
