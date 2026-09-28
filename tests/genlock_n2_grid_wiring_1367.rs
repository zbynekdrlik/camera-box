//! Issue 1367 slice D1 — the WIRING of the grid-exact N>=2 conveyor in the vendored
//! `obs-source.c` (default features, std-only, so it also runs standalone:
//! `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test tests/genlock_n2_grid_wiring_1367.rs`).
//!
//! The decision itself is held by the executable parity gate `tests/genlock_n2_grid_parity_1367.rs`;
//! this pins where it is called from and what the release does with the pick: the ONE branch at
//! the top of `genlock_release_tick` (an N>=2 source never reaches the boundary conveyor), the erase
//! into `genlock_dropped_due`, the `n2_early=` counter, the boundary + consumed + last-frame writes,
//! the video-delay tracker at the scheduled tick, and the late/benign hold split — that the
//! N>=2 boundary conveyor pieces (the #726 multi-consume, the #1161 ACQUIRE bracket) are gone, and
//! that the FIFO drop-cap budgets the grid age of an N>=2 source. Mirrored in both
//! `windows-genlock*.yml` pwsh anchor steps.

use std::path::PathBuf;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The body of the function whose definition starts with `sig`, up to the next top-level `static `.
fn body_after<'a>(src: &'a str, sig: &str) -> &'a str {
    let a = src
        .find(sig)
        .unwrap_or_else(|| panic!("{OBS_SOURCE}: `{sig}` is gone"));
    let rest = &src[a + sig.len()..];
    let b = rest.find(" static ").unwrap_or(rest.len());
    &rest[..b]
}

#[test]
fn an_n2_source_takes_the_grid_release_at_the_top_of_the_tick_1367() {
    let src = squish(&read(OBS_SOURCE));
    let tick = body_after(
        &src,
        "static bool genlock_release_tick(obs_source_t *source, uint64_t wall_now, uint64_t present_ts, size_t due,",
    );
    let branch = "const uint32_t release_n = genlock_effective_source_multiple(source, interval); \
                  if (release_n >= 2) return genlock_release_tick_n2_grid(source, wall_now, present_ts, \
                  interval, reserve_ms, release_n, now_ns);";
    let at = tick.find(branch).unwrap_or_else(|| {
        panic!(
            "{OBS_SOURCE}: issue 1367 D1 — genlock_release_tick no longer sends an N>=2 source to \
             genlock_release_tick_n2_grid first; the boundary conveyor picks its parity by arrival again"
        )
    });
    let conveyor = tick
        .find("size_t release;")
        .expect("the boundary conveyor's release declaration");
    assert!(
        at < conveyor,
        "the N>=2 branch must come before the boundary conveyor"
    );
    assert!(
        !tick.contains("mature_deadline"),
        "issue 1367 D1: the #726 N>=2 multi-consume (mature_deadline) is dead code — an N>=2 \
         source never reaches the STEADY branch"
    );
    assert!(
        !src.contains("genlock_relock_acquire_should_hold")
            && !src.contains("genlock_acquire_bracket_ticks"),
        "issue 1367 D1: the #1161 ACQUIRE bracket gated on N>=2 only — dead once N>=2 leaves the \
         conveyor; its helper, counter and clears are removed"
    );
    assert_eq!(
        src.matches("converge_eligible = true;").count(),
        1,
        "only the N==1 STEADY branch is converge-eligible now"
    );
}

#[test]
fn the_grid_release_erases_counts_and_writes_the_tail_1367() {
    let src = squish(&read(OBS_SOURCE));
    let n2 = body_after(
        &src,
        "static bool genlock_release_tick_n2_grid(obs_source_t *source, uint64_t wall_now, uint64_t present_ts,",
    );
    for (needle, why) in [
        (
            "const uint64_t n2_tick_wall = genlock_n1_tick_wall_now(wall_now);",
            "T is the tick's SCHEDULED wall instant",
        ),
        (
            "const bool n2_on_grid = genlock_n1_tick_is_on_grid(n2_tick_wall, interval);",
            "the on-grid read decides snap vs floor",
        ),
        (
            "genlock_n2_target_stamp_ns(genlock_n2_tick_ns(n2_tick_wall, wall_now, interval, n2_on_grid), (uint64_t)reserve_ms * 1000000ULL, interval, n);",
            "the target from the tick and the pin",
        ),
        (
            "genlock_n2_select(source, n2_target, genlock_n2_source_interval_ns(interval, n));",
            "the pick against the source interval",
        ),
        (
            "if (source->genlock_locked_next_boundary_ns != 0 && present_ts >= source->genlock_locked_next_boundary_ns) source->genlock_late_holds++; else source->genlock_holds++;",
            "a hold is late once locked and aged past the reserve, benign otherwise",
        ),
        (
            "if (pick.kind == GENLOCK_N2_EARLY) source->genlock_n2_early++;",
            "an early present is counted",
        ),
        (
            "da_erase(source->async_frames, 0); remove_async_frame(source, stale); source->genlock_dropped_due++;",
            "every erased frame counts into dropped_due",
        ),
        (
            "source->genlock_locked_next_boundary_ns = next_frame->timestamp + interval;",
            "the boundary is still written",
        ),
        (
            "source->genlock_frames_consumed++; source->last_frame_ts = next_frame->timestamp;",
            "consumed + last_frame_ts",
        ),
        (
            "genlock_video_delay_sample_ns(n2_tick_wall, next_frame->timestamp), interval);",
            "the audio follows the presented age at the scheduled tick",
        ),
        (
            "genlock_shallow_latch(source, n2_tick_wall, wall_now, interval, reserve_ms, false);",
            "an N>=2 tick clears the N==1 shallow state",
        ),
        ("genlock_audit_log(source, now_ns); return true;", "the audit call"),
    ] {
        assert!(
            n2.contains(needle),
            "{OBS_SOURCE}: issue 1367 D1 — genlock_release_tick_n2_grid lost `{needle}` ({why})"
        );
    }
    for gone in [
        "genlock_phase_anchor_ns",
        "genlock_relocks++",
        "genlock_should_converge_phase",
        "genlock_ticks_since_drain",
    ] {
        assert!(
            !n2.contains(gone),
            "{OBS_SOURCE}: issue 1367 D1 — the grid release must not touch `{gone}` (no anchor, \
             relock, converge or drain on an N>=2 source)"
        );
    }
}

#[test]
fn n2_early_is_on_the_audit_line_and_the_source_1367() {
    let src = read(OBS_SOURCE);
    assert!(
        src.contains("\"n2_early=%llu \"")
            && squish(&src).contains(
                "(unsigned long long)source->genlock_n1_grows, (unsigned long long)source->genlock_n2_early,"
            ),
        "{OBS_SOURCE}: issue 1367 D1 — the genlock-fifo audit line must print n2_early= right after \
         n1_grows= (the post-deploy 1 h budget read)"
    );
    let internal = squish(&read(OBS_INTERNAL));
    assert!(
        internal.contains("uint64_t genlock_n2_early;"),
        "{OBS_INTERNAL}: the n2_early counter field is missing"
    );
    // The keys of the REAL audit format string: `n2_early=` right after `n1_grows=`, exactly once,
    // and mutually non-substring with every other key on the line (a `grep -o 'KEY=[0-9]*'` reader
    // must never read one key for another).
    let keys = audit_line_keys(&src);
    assert!(
        keys.len() > 40,
        "the genlock-fifo audit format string was not found whole: {keys:?}"
    );
    let at: Vec<usize> = keys
        .iter()
        .enumerate()
        .filter(|(_, k)| k.as_str() == "n2_early=")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        at.len(),
        1,
        "n2_early= must be on the audit line once: {keys:?}"
    );
    assert_eq!(
        keys[at[0] - 1],
        "n1_grows=",
        "n2_early= must follow n1_grows= on the audit line"
    );
    for other in keys.iter().filter(|k| k.as_str() != "n2_early=") {
        assert!(
            !"n2_early=".contains(other.as_str()) && !other.contains("n2_early="),
            "`n2_early=` collides with the audit key `{other}`"
        );
    }
}

/// The `key=` tokens of the `genlock-fifo audit` format string, in order: the adjacent string
/// literals from `"genlock-fifo audit '%s':` to the trailing ticket list, comments dropped.
fn audit_line_keys(src: &str) -> Vec<String> {
    let start = src
        .find("\"genlock-fifo audit '%s':")
        .unwrap_or_else(|| panic!("{OBS_SOURCE}: the genlock-fifo audit format string is gone"));
    let end = start
        + src[start..]
            .find("\"(#70/")
            .expect("the audit format string's trailing ticket list");
    let mut text = String::new();
    let mut rest = &src[start..end];
    while let Some(i) = rest.find(['"', '/']) {
        let tail = &rest[i..];
        if let Some(body) = tail.strip_prefix("/*") {
            let close = body
                .find("*/")
                .expect("an unterminated comment in the format");
            rest = &body[close + 2..];
        } else if let Some(body) = tail.strip_prefix('"') {
            let close = body
                .find('"')
                .expect("an unterminated literal in the format");
            text.push_str(&body[..close]);
            rest = &body[close + 1..];
        } else {
            rest = &tail[1..];
        }
    }
    text.split_whitespace()
        .filter_map(|w| w.find('=').map(|e| w[..=e].to_string()))
        .collect()
}

/// Review finding (D1): the grid release keeps `50 ms + pin` of frames queued (rounded up to the
/// source grid) plus one canvas interval of arrivals, so an N>=2 source's FIFO drop-cap must add
/// that headroom to the pin's own frame budget — else a deep N>=2 pin (the stream `Zaloha kamera`
/// at 1000 ms) sits on the cap and every push force-drains its whole delay line. The headroom
/// itself is held by the parity gate; this pins that the cap adds it, only for a confirmed N>=2
/// source, before the depth max.
#[test]
fn the_drop_cap_budgets_the_grid_age_of_an_n2_source_1367() {
    let src = squish(&read(OBS_SOURCE));
    let decl = "static inline uint32_t genlock_n2_drop_cap_extra_frames(uint32_t fps_num, uint32_t fps_den);";
    assert_eq!(
        src.matches(decl).count(),
        1,
        "{OBS_SOURCE}: issue 1367 D1 — genlock_source_drop_cap sits above the genlock_n2_* block, so \
         the headroom helper needs its one forward declaration"
    );
    let cap = body_after(
        &src,
        "static size_t genlock_source_drop_cap(const obs_source_t *source) {",
    );
    let add = "if (source->genlock_last_known_n >= 2) { const uint32_t extra = \
               genlock_n2_drop_cap_extra_frames(fps_num, fps_den); latency_frames = extra > \
               UINT32_MAX - latency_frames ? UINT32_MAX : latency_frames + extra; }";
    let at = cap.find(add).unwrap_or_else(|| {
        panic!(
            "{OBS_SOURCE}: issue 1367 D1 — genlock_source_drop_cap no longer adds the N>=2 grid-age \
             headroom (saturating) to a confirmed N>=2 source's frame budget"
        )
    });
    let max = cap
        .find("if (latency_frames > depth) depth = latency_frames;")
        .expect("the depth max of genlock_source_drop_cap");
    assert!(
        at < max,
        "the headroom must be in the budget before the depth max"
    );
    assert!(
        cap.find("if (canvas_frames > latency_frames) latency_frames = canvas_frames;")
            .is_some_and(|c| c < at),
        "the headroom is added after the canvas-rate budget, so it counts once"
    );
}
